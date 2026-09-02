//! `/time` — query, set and add against
//! [`crate::world_state::WorldStateHandle`]'s clock.
//!
//! # `set` writes `day_time`, never `game_time`
//!
//! The real `/time set` never touches the always-advancing game-time counter
//! (see [`crate::world_state`]'s module doc for why that asymmetry exists at
//! all). `/time add` reads the *current* `day_time` and adds to it, so it
//! composes with an intervening `advance_time false`.
//!
//! # The four named values, and why they are not a fifth argument node
//!
//! `day`/`night`/`noon`/`midnight` are registered as literals, each carrying
//! a fixed tick constant, rather than being a `time()` argument's suggestion
//! — a suggestion is text the *player* still has to accept, a literal is a
//! command the tree already resolved. `<time>` is the fifth, numeric child.

use lodestone_command_mc::TimeArg;

use super::registrar::Registrar;
use super::CommandResult;

/// The game-masters permission level.
const TIME_LEVEL: u8 = 2;

/// The real constants for the four named values.
const DAY: i64 = 1_000;
const NOON: i64 = 6_000;
const NIGHT: i64 = 13_000;
const MIDNIGHT: i64 = 18_000;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let time = registrar.literal(root, "time");
    registrar.require_level(time, TIME_LEVEL);

    // ---- set --------------------------------------------------------------
    let set = registrar.literal(time, "set");
    for (name, value) in [("day", DAY), ("noon", NOON), ("night", NIGHT), ("midnight", MIDNIGHT)] {
        let literal = registrar.literal(set, name);
        registrar.exec(literal, move |ctx| set_time(ctx, value));
    }
    let (set_value, set_value_key) = registrar.arg(set, "time", TimeArg::non_negative());
    registrar.exec(set_value, move |ctx| {
        let value = i64::from(*ctx.get(set_value_key));
        set_time(ctx, value)
    });

    // ---- add ----------------------------------------------------------------
    let add = registrar.literal(time, "add");
    let (add_value, add_value_key) = registrar.arg(add, "time", TimeArg::non_negative());
    registrar.exec(add_value, move |ctx| {
        let delta = i64::from(*ctx.get(add_value_key));
        let current = ctx.world.state.time().day_time;
        ctx.world.state.set_day_time(current.saturating_add(delta));
        ctx.send_success(format!("Set the time to {}", ctx.world.state.time().day_time));
        Ok(1)
    });

    // ---- query --------------------------------------------------------------
    // `daytime` is `dayTime % 24000` (what the client renders), `gametime` is
    // the raw always-advancing counter, `day` is whole days elapsed. Three
    // pairwise-distinct expressions over the same clock, so a transposition
    // between them would be visible immediately.
    let query = registrar.literal(time, "query");
    let daytime = registrar.literal(query, "daytime");
    registrar.exec(daytime, |ctx| query_time(ctx, ctx.world.state.time().day_time.rem_euclid(24_000)));
    let gametime = registrar.literal(query, "gametime");
    registrar.exec(gametime, |ctx| query_time(ctx, ctx.world.state.time().game_time));
    let day = registrar.literal(query, "day");
    registrar.exec(day, |ctx| query_time(ctx, ctx.world.state.time().game_time.div_euclid(24_000)));
}

fn query_time(ctx: &mut super::registrar::Ctx<'_>, value: i64) -> CommandResult {
    ctx.send_success(format!("The time is {value}"));
    Ok(i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(i32::MAX))
}

fn set_time(ctx: &mut super::registrar::Ctx<'_>, value: i64) -> CommandResult {
    ctx.world.state.set_day_time(value);
    ctx.send_success(format!("Set the time to {value}"));
    Ok(i32::try_from(value.rem_euclid(24_000)).unwrap_or(0))
}
