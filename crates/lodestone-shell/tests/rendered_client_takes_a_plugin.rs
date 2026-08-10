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

/// A consumer's plugin, in the shape a real one has: it writes `MovementIntent`
/// on the local player every tick, ordered **`.after(TickSet::Intent)`** so its
/// write is the last one that tick.
///
/// # That ordering is the whole fixture, and this file had it wrong
///
/// The first version used a bare `.in_set(TickSet::Intent)` and claimed
/// "`lodestone-autopilot`'s executor does exactly this". **It does not**, and the
/// difference is the defect: `lodestone_autopilot::drive_plan` is
/// `.after(TickSet::Intent).before(TickSet::Physics)`, and its crate docs spell
/// out why — *"after the whole of `TickSet::Intent`, not inside it"*.
///
/// `TickSet::Intent`'s own doc comment names exactly two sanctioned idioms:
/// `.after(TickSet::Intent)` to **override** human input this tick, or
/// `.in_set(TickSet::Intent).after(…)` to compose with it. A bare `.in_set` with
/// no `.after` is neither. It makes the plugin a **second unordered writer of
/// `MovementIntent`** alongside `lodestone_controller::ecs::compute_movement_intent`,
/// which is precisely the shape `lodestone-controller`'s own
/// `a_second_unordered_intent_writer_fails_the_ambiguity_check` exists to catch.
///
/// Measured, not inferred: with the bare `.in_set`, this plugin's system ran all
/// eight ticks and wrote `jump = true` all eight times, and `MovementIntent.jump`
/// read `false` at `TickSet::Predict` on every one of them — `compute_movement_intent`
/// won the race and overwrote it from an idle `RawInput`. The player therefore never
/// moved at all, which surfaced as *"apex 0.0000, horizontal 0.0000"* and reads like
/// a broken seam rather than a fixture that never specified its own order. The race
/// resolved favourably for a while and then stopped; nothing about the source
/// changed when it did, which is why the ordering is now stated rather than lucky.
/// [`the_plugins_intent_is_not_a_second_unordered_writer`] is the mechanical guard.
///
/// **Jump in place, no walk.** Both were tried; see
/// [`a_consumers_plugin_drives_the_rendered_client`] for what each measured and
/// why only this one yields a magnitude the seam alone can explain.
struct JumpPlugin;

impl lodestone_ecs::app::Plugin for JumpPlugin {
    fn build(&self, app: &mut lodestone_ecs::app::App) {
        app.init_resource::<IntentSurvived>();
        app.add_systems(
            lodestone_ecs::GameTick,
            jump.after(TickSet::Intent).before(TickSet::Physics),
        );
        // Reads `MovementIntent` in a set that runs after both writers, so the
        // ordering claim above is observed rather than assumed. `TickSet::Predict`
        // is the first set after `Physics`, which is also after everything in
        // `Intent`.
        app.add_systems(
            lodestone_ecs::GameTick,
            record_surviving_intent.in_set(TickSet::Predict),
        );
    }
}

/// Per-tick record of whether the plugin's `jump = true` was still set once every
/// writer had run — the direct observation of the ordering claim in
/// [`JumpPlugin`]'s docs.
///
/// This exists because the failure it guards is **not** legible from the apex.
/// "Apex 0.0000" is equally consistent with a frozen world, an absent collision
/// view, a plugin that never registered, and this — an intent write that lost a
/// race — and two separate agents spent a session distinguishing them. A gate
/// should say what it measured.
#[derive(bevy_ecs::resource::Resource, Default)]
struct IntentSurvived(Vec<bool>);

fn record_surviving_intent(
    mut survived: ResMut<IntentSurvived>,
    q: Query<&MovementIntent, With<LocalPlayer>>,
) {
    survived
        .0
        .push(q.iter().next().is_some_and(|intent| intent.0.jump));
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

    // Asserted **before** the apex, because it is the assertion that can say why.
    // An apex of 0.0 is consistent with four different faults; this one separates
    // "the plugin's write lost a race" from all of them, and it is the fault this
    // fixture actually had.
    {
        let ecs = sim.ecs().read();
        let survived = &ecs.resource::<IntentSurvived>().0;
        let lost = survived.iter().filter(|s| !**s).count();
        assert_eq!(
            survived.len(),
            60,
            "the plugin's observer must have run once per tick; it ran {} times, so \
             the plugin is not registered in the composed App at all",
            survived.len()
        );
        assert_eq!(
            lost, 0,
            "the plugin's `jump = true` was overwritten on {lost} of {} ticks before \
             physics read it. That is a second unordered writer of `MovementIntent` \
             racing `lodestone_controller::ecs::compute_movement_intent` -- see \
             `JumpPlugin`'s docs. The apex assertion below cannot tell this apart \
             from a frozen world, which is why this one comes first.",
            survived.len()
        );
    }

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

/// **The mechanical guard for [`JumpPlugin`]'s ordering**, so the fixture cannot
/// silently return to being a race.
///
/// `lodestone-controller`'s `exactly_one_system_writes_movement_intent` proves this
/// is detectable: build the schedule with `ambiguity_detection: LogLevel::Error`
/// and an unordered second writer of `MovementIntent` makes
/// `Schedule::initialize` fail.
///
/// # Why this counts pairs instead of asserting "unambiguous"
///
/// **The shell's composed `GameTick` is already ambiguous without any plugin** —
/// measured at **three** conflicting pairs, none of them riding-related and none of
/// them this test's business. A gate that demanded a clean schedule would fail on
/// those and tell nobody anything about the fixture. So the claim is a *delta*:
/// registering `JumpPlugin` must add **zero** new conflicting pairs, while the bare
/// `.in_set(TickSet::Intent)` this fixture used to have adds exactly **one**. That
/// is the negative control, and it is what proves the detector is switched on rather
/// than the first arm passing vacuously.
///
/// The baseline is measured here rather than hardcoded at three, so cleaning up (or
/// adding to) the shell's unrelated ambiguities changes nothing about this gate.
#[test]
fn the_plugins_intent_is_not_a_second_unordered_writer() {
    use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};

    /// A writer with no stated order against `compute_movement_intent` — the shape
    /// `TickSet::Intent`'s doc comment does *not* sanction, and the one this
    /// fixture shipped with.
    fn rogue_unordered(mut q: Query<&mut MovementIntent, With<LocalPlayer>>) {
        for mut intent in &mut q {
            intent.0.jump = true;
        }
    }

    /// Conflicting-access pairs in the composed `GameTick`, under strict detection.
    ///
    /// `initialize` is called on a schedule that has **not** been run: an
    /// already-built schedule is not rebuilt, so the new settings would never be
    /// consulted and this would go vacuous. Same trap
    /// `lodestone_controller::ecs`'s own helper documents.
    fn conflicting_pairs(app: &mut lodestone_ecs::app::App) -> usize {
        app.world_mut()
            .schedule_scope(lodestone_ecs::GameTick, |world, schedule| {
                schedule.set_build_settings(ScheduleBuildSettings {
                    ambiguity_detection: LogLevel::Error,
                    ..ScheduleBuildSettings::default()
                });
                match schedule.initialize(world) {
                    Ok(_) => 0,
                    // The error renders one tuple per pair; counting the tuples is
                    // the only stable handle bevy gives, and a count is what this
                    // gate's verdict depends on.
                    Err(e) => format!("{e}").matches("SystemKey(").count() / 2,
                }
            })
    }

    let baseline = conflicting_pairs(&mut Sim::client_app());

    let mut with_fixture = Sim::client_app();
    with_fixture.add_plugins(JumpPlugin);
    let fixed = conflicting_pairs(&mut with_fixture);

    let mut with_rogue = Sim::client_app();
    with_rogue.add_systems(
        lodestone_ecs::GameTick,
        rogue_unordered.in_set(TickSet::Intent),
    );
    let rogue = conflicting_pairs(&mut with_rogue);

    assert_eq!(
        fixed, baseline,
        "`JumpPlugin` must add no conflicting pair: baseline {baseline}, with the \
         plugin {fixed}. Its system is ordered `.after(TickSet::Intent)` precisely so \
         it does not race `compute_movement_intent`."
    );
    assert_eq!(
        rogue,
        baseline + 1,
        "the negative control must be *detected*: a bare `.in_set(TickSet::Intent)` \
         writer of MovementIntent has to add exactly one conflicting pair (baseline \
         {baseline}, with the rogue {rogue}). If it does not, ambiguity detection is \
         not actually switched on and the arm above proves nothing."
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
