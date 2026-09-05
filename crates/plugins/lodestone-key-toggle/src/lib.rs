//! A toy input-interception plugin: the first consumer of
//! `lodestone_ecs::input`'s `PluginKeybinds`/`PluginKeyEvent` registration
//! point.
//!
//! # What this is
//!
//! On construction, [`KeyTogglePlugin`] claims one physical key in
//! [`lodestone_ecs::KeyInterceptMode::Consume`] and flips a shared boolean
//! every time the shell reports that key pressed — a minimal, real stand-in
//! for the class of client mod the input-interception API targets (a custom
//! hotkey a plugin wants exclusive use of, e.g. a macro tool or a HUD toggle
//! vanilla has no binding for). It exists to prove the substrate end to end
//! — registration
//! reaches `resolve_key`'s precedence chain, a queued raw key event reaches
//! a plugin's `MessageReader` through a real [`lodestone_ecs::GameTick`]
//! tick.
//!
//! While the flag is set, [`extract_marker_when_enabled`] also pushes one
//! world-space [`lodestone_ecs::PluginBillboard`] above the local player's
//! own head every `Extract` — the billboard draw channel, paired with the
//! key-interception registration point, so the toggle does something a
//! plugin author would actually want a
//! hotkey for, not just an internal bool a test can see and nothing else
//! can. See that function's doc for the pairing.
//!
//! # What consumes it, and why that is deliberately not the shipped client
//!
//! Same shape as `lodestone-event-logger` and `lodestone-autopilot`
//! (`crates/plugins/README.md`): this crate is a plugin *library*, not a
//! dependency of `lodestone-shell`. A toggle nobody asked for has no
//! business shipping in every client; the route in for someone who wants it
//! is `Sim::client_app()` + `App::add_plugins` + `Sim::from_app`, the same
//! public composition path any third-party embedder uses.
//!
//! # How it works
//!
//! [`KeyTogglePlugin::new`] returns the plugin *and* a [`KeyToggleState`]
//! handle sharing the same `Arc<AtomicBool>` — the plugin's own system
//! flips it, the caller reads it back, mirroring `EventLoggerPlugin::new`'s
//! `(plugin, handle)` shape and for the identical reason: the toggled state
//! needs to be visible to code outside the `World` (a HUD element, a test),
//! and an `Arc` captured by the system's closure costs
//! `System::initialize` nothing to reach.
//!
//! [`KeyTogglePlugin::build`] does the actual registration:
//! `app.world_mut().resource_mut::<PluginKeybinds>().register(key, Consume)`,
//! after ensuring `LocalPlayerPlugin` (which owns that resource) is
//! installed — the same "add my dependency if it is missing" guard
//! `GameEventBusPlugin::build` already uses for `CorePlugin`. From there the
//! flow is entirely `lodestone_ecs::input`'s own machinery: the shell reads
//! `PluginKeybinds::mode_of` before calling `resolve_key`, gets `Consume`,
//! returns `KeyOutcome::PluginConsumed` (so no gameplay binding sharing the
//! key fires), and separately queues the raw transition into
//! `PendingPluginKeyEvents`. [`drain_pending_plugin_key_events`]
//! (`TickSet::Input`) turns that into a real `Messages<PluginKeyEvent>`
//! this plugin's own system reads with an ordinary `MessageReader`.
//!
//! # How to change it
//!
//! This is intentionally minimal — one key, one boolean. A real macro
//! plugin would probably want several keys and richer per-key actions;
//! both are additive over the one thing this crate exists to demonstrate,
//! which is the `PluginKeybinds` → `resolve_key` → `PendingPluginKeyEvents`
//! → `MessageReader<PluginKeyEvent>` pipeline.
//!
//! # Configuration
//!
//! [`KeyTogglePlugin::new`] takes the [`lodestone_ecs::PhysicalKey`] to
//! claim. No env var, flag or constant.
//!
//! # Dependencies
//!
//! `lodestone-ecs` (the registration/event types, `LocalPlayerPlugin`,
//! `TickSet`, `Extract`/`ExtractSet`, `PluginBillboard(s)`), `bevy_ecs`/`bevy_app`
//! directly (see this crate's `Cargo.toml` for why a plugin that derives its
//! own `Resource`-adjacent type needs them as a direct dependency, not only
//! reachable through `lodestone_ecs::{ecs, app}`), and `lodestone-physics`
//! directly for `Vec3d` (the marker's world-space position), also not
//! reachable through `lodestone-ecs`'s re-exports.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::ecs::prelude::{Commands, Query, Res, ResMut, With};
use lodestone_ecs::player::{LocalPlayer, PhysicsState};
use lodestone_ecs::{
    CameraOverride, Extract, ExtractSet, GameTick, KeyInterceptMode, LocalPlayerPlugin, PhysicalKey,
    PluginBillboard, PluginBillboards, PluginKeyEvent, PluginTexture,
};
use lodestone_physics::Vec3d;

/// A read handle onto a [`KeyTogglePlugin`]'s state. Clones share the same
/// underlying flag — this is a handle, not a snapshot, matching
/// `lodestone-event-logger`'s `EventLog`.
#[derive(Clone, Debug)]
pub struct KeyToggleState(Arc<AtomicBool>);

impl KeyToggleState {
    /// Whether the claimed key has been pressed an odd number of times so
    /// far.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The `Resource` half installed into the `World` so the toggling system can
/// reach it without depending on a `Local<T>` (which would make it
/// unreachable from a second system, and this plugin's design intentionally
/// keeps that door open for `docs/plugin-api.md`'s reasons even though only
/// one system uses it today).
#[derive(Resource, Clone, Debug)]
struct KeyToggleTarget {
    key: PhysicalKey,
    flag: Arc<AtomicBool>,
}

/// Claims `key` in [`KeyInterceptMode::Consume`] on the next tick a
/// `PluginKeyEvent` for it arrives, and flips [`KeyToggleState`]'s flag on
/// every **press** (release edges are ignored — a toggle only wants one
/// transition per physical press, not two).
#[derive(Debug)]
pub struct KeyTogglePlugin {
    key: PhysicalKey,
    flag: Arc<AtomicBool>,
}

impl KeyTogglePlugin {
    /// Build a plugin claiming `key`, plus the [`KeyToggleState`] handle a
    /// caller reads the toggle back through.
    #[must_use]
    pub fn new(key: PhysicalKey) -> (Self, KeyToggleState) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            Self {
                key,
                flag: flag.clone(),
            },
            KeyToggleState(flag),
        )
    }
}

impl Plugin for KeyTogglePlugin {
    fn build(&self, app: &mut App) {
        // `PluginKeybinds` lives on `LocalPlayerPlugin` — install it if a
        // caller has not already, the same "add my dependency if missing"
        // guard `lodestone_ecs::GameEventBusPlugin::build` uses for
        // `CorePlugin`.
        if !app.is_plugin_added::<LocalPlayerPlugin>() {
            app.add_plugins(LocalPlayerPlugin);
        }
        app.world_mut()
            .resource_mut::<lodestone_ecs::PluginKeybinds>()
            .register(self.key.clone(), KeyInterceptMode::Consume);
        app.insert_resource(KeyToggleTarget {
            key: self.key.clone(),
            flag: self.flag.clone(),
        });
        app.add_systems(GameTick, toggle_on_claimed_key_press.in_set(lodestone_ecs::TickSet::Intent));
        app.add_systems(Extract, extract_marker_when_enabled.in_set(ExtractSet::Debug));
    }
}

/// Vertical offset above the local player's feet a toggled marker floats at
/// — enough to sit above head height rather than clipping through the body.
const MARKER_HEIGHT: f64 = 2.2;

/// The marker's footprint, in blocks.
const MARKER_SIZE: [f32; 2] = [0.35, 0.35];

/// A saturated magenta — distinct from `lodestone-autopilot`'s cyan waypoint
/// markers, so the two plugins' billboards never read as the same thing at
/// a glance if both happen to be registered together.
const MARKER_COLOR: [f32; 4] = [1.0, 0.15, 0.9, 0.9];

/// `Extract` / `ExtractSet::Debug`: while [`KeyToggleTarget::flag`] is set,
/// push one [`PluginBillboard`] above the local player's own head — the
/// pairing the key-interception registration point and the billboard draw
/// channel enable together: the claimed key does not just flip an internal bool no one
/// outside a test could observe, it toggles something a plugin author
/// actually wants a hotkey for (a personal marker, a "here" flag for a
/// screen recording, the shape any real HUD-toggle mod needs). One plugin,
/// both APIs, driven end to end by one real key press.
///
/// Reads the same [`KeyToggleTarget`] the toggling system writes rather than
/// [`KeyToggleState`] — the two are the same underlying `Arc<AtomicBool>`,
/// and reading the resource already installed in this `World` keeps this
/// system from needing its own copy of the handle.
fn extract_marker_when_enabled(
    target: Res<KeyToggleTarget>,
    players: Query<&PhysicsState, With<LocalPlayer>>,
    mut billboards: ResMut<PluginBillboards>,
) {
    if !target.flag.load(Ordering::SeqCst) {
        return;
    }
    let Ok(physics) = players.single() else {
        return;
    };
    let p = physics.0.position;
    billboards.0.push(PluginBillboard {
        position: Vec3d::new(p.x, p.y + MARKER_HEIGHT, p.z),
        size: MARKER_SIZE,
        color: MARKER_COLOR,
        texture: PluginTexture::Solid,
    });
}

/// Reads this tick's [`PluginKeyEvent`]s and flips [`KeyToggleTarget::flag`]
/// on a matching **press**. Ordered in `TickSet::Intent` (after
/// `TickSet::Input`, where `drain_pending_plugin_key_events` writes the
/// messages this reads) purely because that is where a plugin that turned a
/// key into `MovementIntent`/`LookIntent` instead would need to run — this
/// system does not itself write either, but the ordering choice documents
/// the intended anchor for anything shaped like this one.
fn toggle_on_claimed_key_press(
    mut events: MessageReader<PluginKeyEvent>,
    target: ResMut<KeyToggleTarget>,
) {
    for event in events.read() {
        if event.pressed && event.key == target.key {
            target.flag.fetch_xor(true, Ordering::SeqCst);
        }
    }
}

/// A shared read handle for [`CameraTogglePlugin`].
///
/// It has the same outside-the-`World` use as [`KeyToggleState`]: a caller can
/// reflect the active director in its own UI without borrowing the simulation.
#[derive(Clone, Debug)]
pub struct CameraToggleState(Arc<AtomicBool>);

impl CameraToggleState {
    /// Whether this plugin currently owns the rendered camera pose.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The one key, pose, and shared state [`CameraTogglePlugin`] needs at tick
/// time. Keeping this in the world lets the system run through ordinary Bevy
/// scheduling rather than asking an input callback to take a world lock.
#[derive(Resource, Clone, Debug)]
struct CameraToggleTarget {
    key: PhysicalKey,
    pose: CameraOverride,
    flag: Arc<AtomicBool>,
}

/// An opt-in, native camera director: a claimed key switches the drawn frame
/// to `pose`; its next press returns immediately to the shell's normal camera.
///
/// This demonstrates the bounded camera-control seam: it changes only
/// [`CameraOverride`], not [`PhysicsState`], the pick ray, or audio. A plugin
/// that needs a moving shot can replace the fixed pose from its own
/// `FrameSet::Camera` system using the same component.
#[derive(Debug)]
pub struct CameraTogglePlugin {
    key: PhysicalKey,
    pose: CameraOverride,
    flag: Arc<AtomicBool>,
}

impl CameraTogglePlugin {
    /// Build a plugin that toggles the rendered camera between the shell pose
    /// and `pose` whenever `key` is pressed.
    #[must_use]
    pub fn new(key: PhysicalKey, pose: CameraOverride) -> (Self, CameraToggleState) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            Self {
                key,
                pose,
                flag: flag.clone(),
            },
            CameraToggleState(flag),
        )
    }
}

impl Plugin for CameraTogglePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<LocalPlayerPlugin>() {
            app.add_plugins(LocalPlayerPlugin);
        }
        app.world_mut()
            .resource_mut::<lodestone_ecs::PluginKeybinds>()
            .register(self.key.clone(), KeyInterceptMode::Consume);
        app.insert_resource(CameraToggleTarget {
            key: self.key.clone(),
            pose: self.pose,
            flag: self.flag.clone(),
        });
        app.add_systems(
            GameTick,
            toggle_camera_on_claimed_key_press.in_set(lodestone_ecs::TickSet::Intent),
        );
    }
}

/// `TickSet::Intent`: give a press edge ownership of the rendered pose, or
/// release it. No local player means there is nothing to claim, so the press is
/// intentionally ignored rather than arming a camera for a later session.
fn toggle_camera_on_claimed_key_press(
    mut commands: Commands,
    mut events: MessageReader<PluginKeyEvent>,
    target: ResMut<CameraToggleTarget>,
    players: Query<bevy_ecs::entity::Entity, With<LocalPlayer>>,
) {
    let Ok(player) = players.single() else {
        return;
    };
    for event in events.read() {
        if event.pressed && event.key == target.key {
            if target.flag.fetch_xor(true, Ordering::SeqCst) {
                commands.entity(player).remove::<CameraOverride>();
            } else {
                commands.entity(player).insert(target.pose);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lodestone_ecs::player::spawn_local_player;
    use lodestone_ecs::{PendingPluginKeyEvents, PluginKeybinds};
    use lodestone_model::Vec3;
    use lodestone_physics::PlayerState;

    use super::*;

    /// Both real APIs, end to end, one plugin: pressing the claimed key
    /// flips [`KeyToggleTarget::flag`] through a real `GameTick`, and a real
    /// `Extract` run afterward carries a marker positioned above the real
    /// local player entity's own [`PhysicsState`] — not a fixture value
    /// restated, the position this test itself set.
    #[test]
    fn enabling_the_toggle_places_a_marker_above_the_local_player() {
        let (plugin, state) = KeyTogglePlugin::new(PhysicalKey::named("KeyM"));
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);

        let player_state = PlayerState::at(Vec3d::new(3.0, 70.0, -5.0), 0.0);
        spawn_local_player(app.world_mut(), player_state);

        // Nothing toggled yet: a real local player exists, but Extract must
        // carry no marker — proves the gate is the flag, not merely
        // "a local player is present".
        app.world_mut().run_schedule(Extract);
        assert!(
            app.world().resource::<PluginBillboards>().0.is_empty(),
            "an untoggled KeyTogglePlugin must draw nothing even with a real local player"
        );

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyM"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(state.enabled(), "the claimed key's press must flip the flag");

        app.world_mut().run_schedule(Extract);
        let billboards = app.world().resource::<PluginBillboards>().0.clone();
        assert_eq!(
            billboards.len(),
            1,
            "an enabled toggle must push exactly one marker"
        );
        assert_eq!(billboards[0].texture, PluginTexture::Solid);
        assert_eq!(billboards[0].color, MARKER_COLOR);
        assert_eq!(
            (billboards[0].position.x, billboards[0].position.z),
            (3.0, -5.0),
            "the marker must sit over the real player's real (x, z)"
        );
        assert!(
            (billboards[0].position.y - (70.0 + MARKER_HEIGHT)).abs() < 1e-9,
            "the marker must float MARKER_HEIGHT above the real player's real y, got {:?}",
            billboards[0].position
        );
    }

    /// The full loop, driven through real machinery rather than called
    /// directly: registering the plugin claims the key in `PluginKeybinds`
    /// (what `resolve_key` would read to swallow it ahead of gameplay —
    /// verified directly here too, closing the loop `lodestone-shell`'s own
    /// `resolve_key` tests only go halfway on since they cannot depend on a
    /// plugin crate), and a raw event queued the way the shell's driver
    /// queues one reaches this plugin's system through one real
    /// `GameTick` and flips the shared flag.
    #[test]
    fn a_queued_press_of_the_claimed_key_flips_the_shared_flag_through_a_real_tick() {
        let (plugin, state) = KeyTogglePlugin::new(PhysicalKey::named("KeyG"));
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);

        // The registration half: `PluginKeybinds` really does carry the
        // claim, in `Consume` mode — the exact fact
        // `lodestone_shell::app::input::resolve_key`'s plugin arm reads.
        assert_eq!(
            app.world()
                .resource::<PluginKeybinds>()
                .mode_of(&PhysicalKey::named("KeyG")),
            Some(KeyInterceptMode::Consume)
        );
        // A different, unclaimed key must not be affected — the same
        // pairwise-distinct-fixture discipline as `lodestone-ecs::input`'s
        // own tests.
        assert_eq!(
            app.world()
                .resource::<PluginKeybinds>()
                .mode_of(&PhysicalKey::named("KeyH")),
            None
        );

        assert!(!state.enabled());

        // Simulate the shell's driver: it would have queued this because
        // `PluginKeybinds::mode_of` returned `Some` above.
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);

        assert!(state.enabled(), "a claimed key's press must flip the flag");

        // Second press toggles back off.
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(!state.enabled(), "a second press must toggle back off");
    }

    /// Negative control: a release edge and an unrelated key must not flip
    /// the flag — proof the positive result above is not a fixture that
    /// flips on any event at all.
    #[test]
    fn a_release_or_an_unrelated_key_does_not_flip_the_flag() {
        let (plugin, state) = KeyTogglePlugin::new(PhysicalKey::named("KeyG"));
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyG"),
                pressed: false,
            });
        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyH"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);

        assert!(!state.enabled());
    }

    /// A camera plugin owns only the rendered-pose component. Its requested
    /// frame must not alter the player's physics pose, and a second press must
    /// remove the component so the shell can resume its normal camera.
    #[test]
    fn camera_toggle_claims_and_releases_only_the_render_pose() {
        let pose = CameraOverride {
            position: Vec3::new(14.0, 80.0, -9.0),
            yaw: 35.0,
            pitch: -20.0,
        };
        let (plugin, state) = CameraTogglePlugin::new(PhysicalKey::named("KeyC"), pose);
        let mut app = bevy_app::App::new();
        app.add_plugins(plugin);
        let player_state = PlayerState::at(Vec3d::new(3.0, 70.0, -5.0), 12.0);
        let player = spawn_local_player(app.world_mut(), player_state);

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyC"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(state.enabled());
        assert_eq!(app.world().get::<CameraOverride>(player), Some(&pose));
        let physics = app.world().get::<PhysicsState>(player).expect("local player physics").0;
        assert_eq!(
            (physics.position, physics.yaw, physics.pitch),
            (player_state.position, player_state.yaw, player_state.pitch),
            "a rendered-camera plugin must not move or rotate the player"
        );

        app.world_mut()
            .resource_mut::<PendingPluginKeyEvents>()
            .0
            .push(PluginKeyEvent {
                key: PhysicalKey::named("KeyC"),
                pressed: true,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(!state.enabled());
        assert!(
            app.world().get::<CameraOverride>(player).is_none(),
            "the second press must hand the rendered camera back to the shell"
        );
    }
}
