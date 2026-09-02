//! `/function <name>` and `/reload` (issue #48's remainder) —
//! `FunctionCommand`/`ReloadCommand`, restated against a real datapack
//! directory scan ([`super::function_store`]) rather than the unit-tested,
//! never-dispatched parser this issue used to leave the feature at.
//!
//! # `/function`'s two forms, and the one real asymmetry between them
//!
//! `/function <name>` (a single function) and `/function #<tag>` (every
//! function a tag names) share one executor, keyed on
//! [`lodestone_command_mc::FunctionRef`]. They differ in exactly one way,
//! matching vanilla: **an unknown single function is a hard refusal**
//! (`FunctionArgument.ERROR_UNKNOWN_FUNCTION`), while **an unknown tag is not
//! an error at all** — vanilla's own `getTag` answers `List.of()` for a tag
//! no loaded datapack declares, so `/function #foo:nothing` reports "Ran 0
//! functions" rather than refusing. [`super::function_store::FunctionHandle`]
//! mirrors that split: [`super::function_store::FunctionHandle::function`]
//! returns `Option`, [`super::function_store::FunctionHandle::tag`] never
//! does.
//!
//! # A function body runs through the real dispatcher, one line per line
//!
//! Each command line reaches [`super::registrar::Ctx::run_command`] — the
//! same tree this file's own executors sit on, built-ins first and the
//! host's contextual dispatch on a miss, exactly `/execute … run`'s own
//! fallback. A line's own failure does **not** stop the rest of the
//! function: vanilla's `NewCommandFunctionManager` dispatches every command
//! in a function independently, catching and logging each one's own
//! `CommandSyntaxException` rather than aborting the function — restated
//! here as "count successes, keep going regardless of a line's own result".
//!
//! # The recursion guard, and why vanilla's own limit does not apply here
//!
//! A function that calls itself (directly, or through a tag cycle) recurses
//! through this file's own Rust call stack — `run_command` re-enters
//! [`super::ServerCommands::run`], which re-enters this executor — unlike
//! vanilla, whose function execution is an explicit queue bounded by the
//! `maxCommandChainLength` gamerule (65536 by default) rather than by a
//! native call stack. Reusing that number here would overflow an ordinary
//! thread's stack long before it fired, so [`MAX_FUNCTION_DEPTH`] is a much
//! smaller, purely defensive bound with no vanilla counterpart, chosen only
//! to turn a runaway recursive datapack function into a clean refusal
//! instead of a crash.
//!
//! # What is not built
//!
//! **Macro functions** (`$name` lines, a `with <storage>`/NBT argument) —
//! see [`super::function_store`]'s own doc for why a `$` line is read and
//! silently skipped rather than expanded: no built-in command surface here
//! supplies the substitution source such a call would need. **`/function
//! <name> <arguments>`'s macro-argument form is therefore unregistered
//! entirely** — only the bare `<name>` form exists.

use std::cell::Cell;

use lodestone_command_mc::FunctionRef;

use super::registrar::{CommandResult, Ctx, Registrar};

/// `Commands.LEVEL_GAMEMASTERS` — vanilla's own level for both
/// `FunctionCommand` and `ReloadCommand`.
const FUNCTION_LEVEL: u8 = 2;

/// A purely defensive bound on nested `/function` calls — see this module's
/// doc for why vanilla's own `maxCommandChainLength` does not transfer here.
const MAX_FUNCTION_DEPTH: u32 = 256;

thread_local! {
    static FUNCTION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// RAII: increments the thread-local depth counter on construction, restores
/// it on drop (including on an unwind), so a refusal or an early `?` inside
/// the guarded region can never leave the counter incremented forever.
struct DepthGuard;

impl Drop for DepthGuard {
    fn drop(&mut self) {
        FUNCTION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn enter_function_depth() -> Result<DepthGuard, String> {
    let depth = FUNCTION_DEPTH.with(Cell::get);
    if depth >= MAX_FUNCTION_DEPTH {
        return Err(format!(
            "Function call depth exceeded ({MAX_FUNCTION_DEPTH}) — check for infinite recursion in your datapack"
        ));
    }
    FUNCTION_DEPTH.with(|d| d.set(depth + 1));
    Ok(DepthGuard)
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();

    let function_lit = registrar.literal(root, "function");
    registrar.require_level(function_lit, FUNCTION_LEVEL);
    let (name_node, name_key) =
        registrar.arg(function_lit, "name", lodestone_command_mc::FunctionArg);
    registrar.exec(name_node, move |ctx| {
        let reference = ctx.get(name_key).clone();
        let result = run_function_ref(ctx, &reference);
        // `FunctionCommand.run`'s own `sendSuccess` line — added at this one
        // outer call only, not inside the recursive helper, so a function
        // that itself calls `/function` another does not get one summary
        // per nesting level.
        if let Ok(count) = result {
            ctx.send_success(format!("Ran {count} command(s) from function"));
        }
        result
    });

    let reload_lit = registrar.literal(root, "reload");
    registrar.require_level(reload_lit, FUNCTION_LEVEL);
    registrar.exec(reload_lit, move |ctx| {
        match ctx.world.state.functions().reload() {
            Some(report) => {
                ctx.send_success(format!(
                    "Reloaded {} function(s) and {} tag(s)",
                    report.functions, report.tags
                ));
                Ok(1)
            }
            None => {
                // No datapacks directory was ever configured for this world
                // (RCON with no world source, an in-memory/browser world) —
                // an honest no-op, not an error: there is genuinely nothing
                // to reload, matching a fresh singleplayer world with zero
                // datapacks reporting the same thing vanilla would (a
                // reload that finds nothing changed).
                ctx.send_success("No datapacks are configured for this world");
                Ok(0)
            }
        }
    });
}

/// Runs a single function or an entire tag, under the recursion guard. See
/// this module's doc for the guard's rationale and for the
/// unknown-function-vs-unknown-tag asymmetry.
fn run_function_ref(ctx: &mut Ctx<'_>, reference: &FunctionRef) -> CommandResult {
    let _guard = enter_function_depth()?;
    match reference {
        FunctionRef::Single(id) => run_one_function(ctx, &id.to_string()),
        FunctionRef::Tag(id) => {
            let ids = ctx.world.state.functions().tag(&id.to_string());
            let mut total = 0;
            for function_id in ids {
                // A tag runs every member regardless of any one member's own
                // outcome — the same "keep going" rule a single function's
                // own lines already follow, one level up.
                total += run_one_function(ctx, &function_id).unwrap_or(0);
            }
            Ok(total)
        }
    }
}

/// Runs one concrete function's lines. `Err` only for a name no loaded
/// datapack declares at all — every other outcome (some or all lines
/// failing) is `Ok` with the count of lines that actually ran.
fn run_one_function(ctx: &mut Ctx<'_>, id: &str) -> CommandResult {
    let Some(lines) = ctx.world.state.functions().function(id) else {
        return Err(format!("Unknown function {id}"));
    };
    let mut count = 0;
    for line in &lines {
        if ctx.run_command(line).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}
