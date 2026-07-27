//! Browser input platform layer, driving the **shared** player controller.
//!
//! ## What this is — and what stays browser-specific
//!
//! This module is the browser's *platform input layer*: the peer of the native
//! shell's winit `app.rs`. Turning raw DOM events (`KeyboardEvent`, pointer-lock
//! mouse motion) into a logical held-key snapshot is inherently platform
//! specific — every platform needs its own event-source adapter — so this half
//! is not a fork of anything and has a native counterpart by design.
//!
//! ## The controller is now shared (W-next)
//!
//! The gameplay decisions — the held-key model, the vanilla cubic mouse-look
//! curve, the forward-gated sprint rule, the `-90..90` pitch clamp — live in the
//! wasm-safe [`lodestone_controller`] crate and are consumed here **unchanged**,
//! the same functions native `lodestone-shell` calls. There is now exactly one
//! [`movement_intent`] / [`apply_look`] implementation across both platforms, so
//! neither can grow its own and drift from vanilla.
//!
//! (The earlier W6 blocker — that the logic was welded to native-only deps via
//! `lodestone-shell` → `lodestone-client` → tokio `net` → `mio` — was resolved
//! by `impl-shell` lifting it into `lodestone-controller`, which depends only on
//! `lodestone-physics` + `lodestone-client` and compiles to wasm.)
//!
//! ## Why the camera drive is still free-fly, and why that is NOT a fork
//!
//! [`FlyCamera`] feeds the shared [`InputState`] through the shared
//! [`movement_intent`] / [`apply_look`], but integrates the resulting
//! [`MovementInput`](lodestone_physics::MovementInput) as a **free-fly** move
//! (no gravity, no collision) rather than through `lodestone_physics::tick`.
//! That is deliberate and mirrors the native shell's own `fly_tick`, which is
//! also shell-local and also reads the shared `MovementInput` — free-fly is a
//! viewer, honestly a different, simpler thing than walking, so it lives in the
//! platform layer on *both* sides, not in the controller.
//!
//! The reason the browser can't yet run real physics-walk is **not** the
//! controller: it holds greedy-meshed GPU buffers, not a `lodestone_world::World`
//! to collide against. Real `physics_tick` plus the `move_action` wire lowering
//! arrive together with the `chunk(pos) -> LoadedChunk` decode seam and a live
//! `ClientHandle` — version/client work, not this crate.

use std::cell::RefCell;
use std::rc::Rc;

use glam::Vec3;
use lodestone_controller::{Action, InputState, apply_look, movement_intent};
use lodestone_render::Camera;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlCanvasElement, KeyboardEvent, MouseEvent};

/// Vanilla's mouse-sensitivity slider (`0..1`); `0.5` is the game's default and
/// yields exactly `0.15°`/pixel through the shared cubic curve.
const SENSITIVITY: f32 = 0.5;

struct Inner {
    /// The shared controller's held-action + accumulated-mouse state. Replaces
    /// the browser's former private held-key struct so there is one model.
    input: InputState,
    /// Whether the canvas currently holds the pointer lock.
    locked: bool,
}

/// Shared, cheaply-clonable handle to the browser input state. The DOM event
/// callbacks and the render loop both hold one.
#[derive(Clone)]
pub struct Controls {
    inner: Rc<RefCell<Inner>>,
}

impl Controls {
    fn new() -> Self {
        Controls {
            inner: Rc::new(RefCell::new(Inner {
                input: InputState::default(),
                locked: false,
            })),
        }
    }

    /// Snapshot the held-action state and take (clear) the accumulated mouse
    /// motion. [`InputState`] is `Copy`, so the render loop gets a detached
    /// snapshot it can feed to the shared controller functions.
    fn take(&self) -> (InputState, (f32, f32)) {
        let mut i = self.inner.borrow_mut();
        let mouse = i.input.take_mouse();
        (i.input, mouse)
    }
}

/// Map a physical `KeyboardEvent.code` to a shared-controller [`Action`].
///
/// **Hazard handled:** we key off `event.code()` (the physical key position,
/// e.g. `"KeyW"`) and never `event.key()` (the produced character, which is
/// layout-dependent: AZERTY, Dvorak, and IME states all move `'w'`). A WASD
/// scheme keyed on characters silently breaks for a large fraction of the
/// world; keyed on `code` it is stable across layouts.
fn code_to_action(code: &str) -> Option<Action> {
    Some(match code {
        "KeyW" => Action::Forward,
        "KeyS" => Action::Back,
        "KeyA" => Action::Left,
        "KeyD" => Action::Right,
        "Space" => Action::Jump,
        "ShiftLeft" | "ShiftRight" => Action::Sneak,
        "ControlLeft" | "ControlRight" => Action::Sprint,
        _ => return None,
    })
}

/// A free-fly viewer camera the render loop advances each frame, driven through
/// the **shared** controller (look + input semantics) with a browser-local
/// free-fly integration (see the module docs for why that split is correct).
pub struct FlyCamera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

impl FlyCamera {
    /// Start looking at `target` from `position`, deriving the initial yaw/pitch
    /// so the very first frame matches where the old auto-orbit camera sat.
    pub fn looking_at(position: Vec3, target: Vec3) -> Self {
        let d = (target - position).normalize_or_zero();
        // Invert Camera::forward(): fwd = (-sin(yaw)cos(pitch), -sin(pitch),
        // cos(yaw)cos(pitch)); positive pitch looks down.
        let pitch = (-d.y).asin().to_degrees();
        let yaw = (-d.x).atan2(d.z).to_degrees();
        FlyCamera {
            position,
            yaw,
            pitch,
        }
    }

    /// Advance the pose from the current input and build the render `Camera`.
    ///
    /// `dt` is the frame delta in seconds so movement is frame-rate independent.
    /// Look integration uses the shared [`apply_look`] (vanilla cubic
    /// sensitivity, `-90..90` pitch clamp); movement direction uses the shared
    /// [`movement_intent`] (forward-gated sprint, cancel-on-opposite). Only the
    /// gravity-free/collision-free *integration* below is browser-local.
    pub fn advance(&mut self, controls: &Controls, dt: f32, aspect: f32) -> Camera {
        let (state, (dx, dy)) = controls.take();

        // Shared look: vanilla cubic sensitivity + pitch clamp + yaw wrap.
        let (yaw, pitch) = apply_look(self.yaw, self.pitch, dx, dy, SENSITIVITY);
        self.yaw = yaw;
        self.pitch = pitch;

        let mut cam = Camera {
            position: self.position,
            yaw,
            pitch,
            fov_y_degrees: 70.0,
            aspect,
            near: 0.1,
            far: 4000.0,
        };

        // Shared input semantics → browser-local free-fly integration. This is
        // the same shape as native `fly_tick`: read the shared `MovementInput`,
        // move along the camera basis, ignore gravity/collision. Real
        // physics-walk arrives with the World-decode seam, not here.
        let intent = movement_intent(&state);
        let fwd = cam.forward();
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let mut dir = Vec3::ZERO;
        // `forward` is +1 with W. `strafe` is +1 with **left** (A) by the
        // controller's convention, so +strafe moves along -right.
        dir += fwd * intent.forward;
        dir -= right * intent.strafe;
        if intent.jump {
            dir += Vec3::Y;
        }
        if intent.sneak {
            dir -= Vec3::Y;
        }
        // Free-fly boost uses the RAW (ungated) sprint, exactly like native
        // `fly_tick`: the forward-only sprint gate is a walking rule, and a
        // viewer wants to boost in any direction.
        let speed = if state.sprint_held() { 80.0 } else { 24.0 };
        self.position += dir.normalize_or_zero() * speed * dt;
        cam.position = self.position;
        cam
    }
}

/// Install the browser input platform layer: pointer-lock lifecycle, keyboard,
/// and mouse-look listeners. Returns the shared [`Controls`] the render loop
/// reads. `status` is a HUD-line setter so lock refusals are visible, not silent.
pub fn install(
    canvas: &HtmlCanvasElement,
    doc: &Document,
    status: impl Fn(&str) + 'static,
) -> Controls {
    let controls = Controls::new();
    let status = Rc::new(status);

    // Click the canvas to capture the mouse. Pointer lock *must* be requested
    // from a user gesture, so this is the only place it can start.
    {
        let canvas_c = canvas.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            // Fire-and-forget; refusal surfaces via the pointerlockerror handler.
            canvas_c.request_pointer_lock();
        });
        let _ = canvas
            .add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // Pointer-lock state changes. **Hazard handled:** the user can drop the lock
    // at any moment (Esc, Alt-Tab, losing focus). When that happens we clear all
    // held keys so the camera doesn't keep drifting, and tell the player how to
    // resume — a stuck-moving camera after Alt-Tab is the classic browser FPS bug.
    {
        let controls_c = controls.clone();
        let doc_c = doc.clone();
        let status_c = status.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            // This app only ever requests the lock on its own canvas, so any
            // lock element present in the document is ours.
            let locked = doc_c.pointer_lock_element().is_some();
            let mut i = controls_c.inner.borrow_mut();
            i.locked = locked;
            if !locked {
                // Clear held actions so the camera doesn't keep drifting, and
                // discard any pending mouse so re-locking doesn't jump the view.
                i.input.release_all();
                i.input.take_mouse();
                drop(i);
                status_c("mouse released — click the scene to look around · WASD move · Shift/Space down/up · Ctrl boost");
            } else {
                drop(i);
                status_c("mouse captured — WASD move · mouse look · Shift/Space down/up · Ctrl boost · Esc to release");
            }
        });
        let _ = doc.add_event_listener_with_callback(
            "pointerlockchange",
            cb.as_ref().unchecked_ref(),
        );
        cb.forget();
    }

    // Pointer-lock refusal. **Hazard handled:** the request can be denied (e.g.
    // the document isn't focused, or the browser rate-limits re-entry after Esc).
    // A silent refusal would look like broken input; we say so instead.
    {
        let status_c = status.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            status_c("pointer lock refused by the browser — click the scene again (must be focused)");
        });
        let _ = doc
            .add_event_listener_with_callback("pointerlockerror", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // Mouse motion: only meaningful under pointer lock, where `movementX/Y` are
    // raw deltas (absolute coords are frozen). We accumulate; the render loop
    // consumes once per frame so fast mice don't over-sample the sim.
    {
        let controls_c = controls.clone();
        let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let mut i = controls_c.inner.borrow_mut();
            if i.locked {
                i.input.add_mouse(e.movement_x() as f32, e.movement_y() as f32);
            }
        });
        let _ = doc
            .add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // Keydown/keyup on the document. We map physical `code`s (see
    // `code_to_action`) to shared-controller `Action`s and `preventDefault` the
    // ones the browser would otherwise eat (Space scrolls the page; Ctrl/Shift
    // can trigger shortcuts) — but only while locked, so normal page use outside
    // the scene is untouched.
    {
        let controls_c = controls.clone();
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let mut i = controls_c.inner.borrow_mut();
            let locked = i.locked;
            if let Some(action) = code_to_action(&e.code()) {
                i.input.set(action, true);
                if locked {
                    e.prevent_default();
                }
            }
        });
        let _ = doc
            .add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        cb.forget();
    }
    {
        let controls_c = controls.clone();
        let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let mut i = controls_c.inner.borrow_mut();
            if let Some(action) = code_to_action(&e.code()) {
                i.input.set(action, false);
            }
        });
        let _ = doc
            .add_event_listener_with_callback("keyup", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    controls
}
