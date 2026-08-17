//! `/stopwatch`, from `StopwatchCommand.java` (issue #48's remainder) — the
//! producer `/execute if`/`unless stopwatch` reads.
//!
//! # What it is
//!
//! Four subcommands over [`crate::commands::stopwatch_store::StopwatchHandle`]:
//! `create <id>` (refuses a duplicate id), `query <id> [<scale>]` (reports
//! elapsed seconds, returning `elapsed_seconds * scale` truncated to `i32`,
//! `scale` defaulting to `1.0`), `restart <id>` (a hard reset — see the
//! store's own doc for why that is not a pause/resume), and `remove <id>`.
//! Each of the three id-taking subcommands other than `create` refuses an
//! unknown id by name, matching vanilla's own `ERROR_DOES_NOT_EXIST`.
//!
//! See the store's own module doc for what this deliberately does not do
//! (persist across a restart) and why.

use lodestone_command::DoubleArgument;
use lodestone_command_mc::IdentifierArg;

use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const STOPWATCH_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let stopwatch = registrar.literal(root, "stopwatch");
    registrar.require_level(stopwatch, STOPWATCH_LEVEL);

    let create = registrar.literal(stopwatch, "create");
    let (create_id_node, create_id_key) = registrar.arg(create, "id", IdentifierArg);
    registrar.exec(create_id_node, move |ctx| {
        let id = ctx.get(create_id_key).to_string();
        if ctx.world.state.stopwatches().create(&id) {
            ctx.send_success(format!("Created new stopwatch: {id}"));
            Ok(1)
        } else {
            Err(format!("A stopwatch already exists by that name: {id}"))
        }
    });

    let query = registrar.literal(stopwatch, "query");
    let (query_id_node, query_id_key) = registrar.arg(query, "id", IdentifierArg);
    registrar.exec(query_id_node, move |ctx| run_query(ctx, query_id_key, 1.0));
    let (scale_node, scale_key) = registrar.arg(query_id_node, "scale", DoubleArgument::new());
    registrar.exec(scale_node, move |ctx| {
        let scale = *ctx.get(scale_key);
        run_query(ctx, query_id_key, scale)
    });

    let restart = registrar.literal(stopwatch, "restart");
    let (restart_id_node, restart_id_key) = registrar.arg(restart, "id", IdentifierArg);
    registrar.exec(restart_id_node, move |ctx| {
        let id = ctx.get(restart_id_key).to_string();
        if ctx.world.state.stopwatches().restart(&id) {
            ctx.send_success(format!("Restarted stopwatch: {id}"));
            Ok(1)
        } else {
            Err(format!("No stopwatch exists by that name: {id}"))
        }
    });

    let remove = registrar.literal(stopwatch, "remove");
    let (remove_id_node, remove_id_key) = registrar.arg(remove, "id", IdentifierArg);
    registrar.exec(remove_id_node, move |ctx| {
        let id = ctx.get(remove_id_key).to_string();
        if ctx.world.state.stopwatches().remove(&id) {
            ctx.send_success(format!("Removed stopwatch: {id}"));
            Ok(1)
        } else {
            Err(format!("No stopwatch exists by that name: {id}"))
        }
    });
}

/// `StopwatchCommand.queryStopwatch` — shared by the bare `query <id>` form
/// (`scale` defaulting to `1.0`) and the explicit-scale form.
fn run_query(
    ctx: &mut Ctx<'_>,
    id_key: super::registrar::ArgKey<lodestone_model::ids::ResourceKey>,
    scale: f64,
) -> CommandResult {
    let id = ctx.get(id_key).to_string();
    let Some(elapsed) = ctx.world.state.stopwatches().elapsed_seconds(&id) else {
        return Err(format!("No stopwatch exists by that name: {id}"));
    };
    ctx.send_success(format!("Stopwatch {id}: {elapsed:.3}s"));
    #[allow(clippy::cast_possible_truncation)]
    Ok((elapsed * scale) as i32)
}
