//! `/weather` — the mechanism this command needed was a way to reach the
//! tick loop's lock-free [`crate::weather::WeatherState`] from a command
//! executor, which cannot borrow it directly (it is owned by
//! `crate::tick::run_tick_loop_with_weather` with no lock at all — the same
//! reason [`crate::sleep::SleepVote`] is a separate shared handle rather than
//! a direct `SleepState` borrow). [`crate::world_state::WorldStateHandle::request_weather`]
//! is that reach: a caller-side request the loop consults once per pass and
//! applies immediately (see that method's own doc), mirroring `SleepVote`'s
//! split rather than adding a second lock.
//!
//! # `-1` (the real "sample a default duration") is a fixed constant here
//!
//! The real rule falls back to a seeded random sample from the rain-delay/
//! rain-duration range when no `<duration>` is given. This crate has no
//! per-world seeded random source reachable from a command executor — the
//! same gap `crate::commands::experience`'s `/xp query` names for a
//! different store — so the bare-literal forms use [`DEFAULT_DURATION`]
//! instead of a sampled one. A documented approximation, not a faithfulness
//! bug: any duration inside the real 12,000-180,000 tick range produces a
//! real, visible transition, and this one is the midpoint of the rain
//! duration's narrower 12,000-24,000 range.

use lodestone_command_mc::TimeArg;

use super::registrar::Registrar;
use super::CommandResult;
use crate::world_state::WeatherRequest;

/// The game-masters permission level.
const WEATHER_LEVEL: u8 = 2;

/// Stand-in for the real sampled default duration — see the module doc.
/// The real rain-duration range is 12,000-24,000 ticks; this is the midpoint.
const DEFAULT_DURATION: i32 = 18_000;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let weather = registrar.literal(root, "weather");
    registrar.require_level(weather, WEATHER_LEVEL);

    register_kind(registrar, weather, "clear", "clear", |duration| WeatherRequest::Clear { duration });
    register_kind(registrar, weather, "rain", "rain", |duration| WeatherRequest::Rain { duration });
    register_kind(registrar, weather, "thunder", "thunder", |duration| WeatherRequest::Thunder {
        duration,
    });
}

fn register_kind(
    registrar: &mut Registrar,
    weather: lodestone_command::NodeId,
    literal_name: &str,
    label: &'static str,
    make: impl Fn(i32) -> WeatherRequest + Copy + Send + Sync + 'static,
) {
    let kind = registrar.literal(weather, literal_name);
    registrar.exec(kind, move |ctx| apply(ctx, make(DEFAULT_DURATION), label));

    // A minimum of 1: no negative or zero-length spell.
    let (dur_node, dur_key) = registrar.arg(kind, "duration", TimeArg { min: 1 });
    registrar.exec(dur_node, move |ctx| {
        let duration = *ctx.get(dur_key);
        apply(ctx, make(duration), label)
    });
}

fn apply(ctx: &mut super::registrar::Ctx<'_>, request: WeatherRequest, label: &str) -> CommandResult {
    ctx.world.state.request_weather(request);
    ctx.send_success(format!("Set the weather to {label}"));
    Ok(1)
}
