//! The gate that milestone zero is actually about: **the shipped graphical
//! client can now take a plugin.**
//!
//! Before this, `Sim::build` called `App::new()` itself, added a fixed tuple,
//! then `std::mem::take`d the `World` and dropped the `App` — and since
//! `Plugin::build` needs `&mut App`, no downstream crate holding a `Sim` could
//! register one. `Sim::ecs()` handing out `&mut World` did not help: there is no
//! supported way to merge one `App`'s `Schedules` into another `World`. So the
//! headless half of the seam existed (`ClientBuilder::ecs`) and the rendered half
//! did not. This test is the rendered half.
//!
//! # What it proves, and why a marker plugin rather than the autopilot
//!
//! The plugin here is not a stub that only sets a flag. [`JumpPlugin`] does
//! structurally what `lodestone-autopilot` does — a `GameTick` system in
//! `TickSet::Intent` writing `MovementIntent` on the local player — and the
//! assertion is that the **player's position actually changes** when the real
//! `Sim::step` driver runs. Registration is proven by the movement, not by
//! `is_plugin_added`.
//!
//! It is not `AutopilotPlugin` itself for one deliberate reason:
//! `crates/lodestone-shell/Cargo.toml` states that `lodestone-autopilot` must not
//! become a dependency of this crate — "not even `optional = true` behind a
//! feature" — and a `[dev-dependencies]` entry would put an LGPL-3.0-or-later
//! crate into the MIT-OR-Apache-2.0 engine's own test graph and version-lock the
//! plugin's gates to the shell. The autopilot's real goal-arrival behaviour
//! *through the same seam* is gated one crate down, in
//! `crates/lodestone-app/tests/headless_consumer_registers_a_plugin.rs`, where
//! the dependency direction is plugins→engine as `crates/plugins/README.md`
//! requires. Between the two: the composed `App` runs a genuinely external
//! plugin to its goal, and the rendered `Sim` runs a caller's plugin's systems.
//!
//! # The negative control
//!
//! [`without_the_plugin_the_player_stays_put`] is the identical `Sim`, the
//! identical `step` budget, `JumpPlugin` **not** added — and the player must
//! not move. Without it, a demo-world `Sim` whose player slid down a slope for
//! any unrelated reason would read as a pass; the demo world has terrain, so this
//! is a live risk rather than a formality.

use bevy_ecs::prelude::*;
use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_ecs::TickSet;
use lodestone_ecs::player::{LocalPlayer, MovementIntent};

/// A consumer's plugin, in the shape a real one has: it claims
/// `TickSet::Intent` and writes `MovementIntent` on the local player every tick.
/// `lodestone-autopilot`'s executor does exactly this, one set after
/// `ControllerPlugin`'s human-input write in `TickSet::Input`, which is what
/// makes a plugin's intent survive to `TickSet::Physics`.
///
/// **Jump in place, no walk.** Both were tried; see
/// [`a_consumers_plugin_drives_the_rendered_client`] for what each measured and
/// why only this one yields a magnitude the seam alone can explain.
struct JumpPlugin;

impl lodestone_ecs::app::Plugin for JumpPlugin {
    fn build(&self, app: &mut lodestone_ecs::app::App) {
        app.add_systems(lodestone_ecs::GameTick, jump.in_set(TickSet::Intent));
    }
}

fn jump(mut q: Query<&mut MovementIntent, With<LocalPlayer>>) {
    for mut intent in &mut q {
        intent.0.forward = 0.0;
        intent.0.strafe = 0.0;
        intent.0.jump = true;
        intent.0.sneak = false;
        intent.0.sprint = false;
    }
}

/// `Mode::Headless` so the `Sim` builds the offline demo world and the player has
/// real, jar-independent ground to walk on — the same fixture every hermetic
/// `sim` gate uses. `render_distance: 2` keeps the generated radius small.
fn test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

/// Drive `ticks` real ticks and report the highest the player's feet ever got
/// above where they started, plus the final horizontal displacement.
fn walk_and_measure(sim: &mut Sim, ticks: u32) -> (f64, f64) {
    let start = sim.player().position;
    let mut apex = 0.0f64;
    for _ in 0..ticks {
        sim.step(1.0 / 20.0);
        apex = apex.max(sim.player().position.y - start.y);
    }
    let now = sim.player().position;
    let horizontal = ((now.x - start.x).powi(2) + (now.z - start.z).powi(2)).sqrt();
    (apex, horizontal)
}

/// Milestone zero's rendered-path gate.
///
/// # Why a jump in place, and what the two rejected observables measured
///
/// Both alternatives were run, and each failed for a reason worth recording
/// rather than for the reason the test exists:
///
/// | plugin writes | measured over 60 ticks | why it is the wrong observable |
/// |---|---|---|
/// | `forward = 1.0` | horizontal **0.2000**, apex 0.0 | the plugin was already provably driving the player (the control moved 0.0000), but the demo world's spawn column is not a clear corridor and the walk stalled against a step within two ticks — the assertion measures the fixture's geometry |
/// | `forward = 1.0, jump = true` | horizontal **6.4733**, apex **4.1661** | it walked *up a hill*, so the apex is terrain height plus a jump and predicts nothing |
/// | `jump = true` alone | apex ≈ vanilla's 1.2522 | no horizontal travel, so no terrain to climb: the apex is the jump and only the jump |
///
/// The third row is a **magnitude** prediction rather than a direction one, which
/// is what this repo's evidence standards ask for. Vanilla's jump rises ~1.2522
/// blocks from an initial 0.42 b/t against 0.08 b/t² gravity — constants with
/// nothing to do with the plugin seam — so the window below rejects a player
/// merely nudged upward by a collision resolution as firmly as it rejects one
/// that never left the ground. The second row is also the affirmative answer to
/// "does a plugin's *walk* reach the rendered client": 6.47 blocks against the
/// control's 0.0000.
#[test]
fn a_consumers_plugin_drives_the_rendered_client() {
    let mut app = Sim::client_app();
    app.add_plugins(JumpPlugin);
    let mut sim = Sim::from_app(app, test_config());

    let (apex, horizontal) = walk_and_measure(&mut sim, 60);

    assert!(
        (0.9..1.6).contains(&apex),
        "a plugin registered through `Sim::client_app()` + `Sim::from_app` must actually \
         drive the rendered client's local player: expected a vanilla jump apex near \
         1.2522 blocks, measured {apex:.4} (horizontal displacement was {horizontal:.4})"
    );
    assert!(
        horizontal < 0.1,
        "premise: a jump-in-place plugin must not travel, or the apex above is measuring \
         terrain rather than the jump; horizontal displacement was {horizontal:.4}"
    );
}

/// The control. Identical `Sim`, identical budget, no plugin — so nothing writes
/// `MovementIntent` and the player must neither rise nor move.
#[test]
fn without_the_plugin_the_player_stays_put() {
    let mut sim = Sim::from_app(Sim::client_app(), test_config());

    let (apex, horizontal) = walk_and_measure(&mut sim, 60);

    assert!(
        apex < 0.05,
        "control: with no plugin registered nothing may lift the player, yet the apex was \
         {apex:.4} blocks — so the positive assertion above is not measuring the plugin"
    );
    assert!(
        horizontal < 0.05,
        "control: with no plugin registered nothing may move the player horizontally, yet \
         displacement was {horizontal:.4} blocks"
    );
}

/// `Sim::new` must still be `client_app()` + `from_app` and nothing else: the two
/// routes have to produce the same starting state, or the shell has kept a
/// private composition path after all and the seam can drift.
#[test]
fn from_app_and_new_agree_on_the_starting_state() {
    let via_new = Sim::new(test_config());
    let via_seam = Sim::from_app(Sim::client_app(), test_config());

    assert_eq!(
        via_new.player().position,
        via_seam.player().position,
        "`Sim::new` and the public seam must start the player in the same place"
    );
    assert_eq!(
        via_new.chunk_count(),
        via_seam.chunk_count(),
        "`Sim::new` and the public seam must build the same world"
    );
}
