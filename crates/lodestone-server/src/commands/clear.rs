//! `/clear`, from `ClearInventoryCommand.java`.
//!
//! # `item` is [`lodestone_command_mc::ItemArg`], not `ItemPredicateArgument`
//!
//! Vanilla's `<item>` node accepts an item **tag** (`#minecraft:...`) as well
//! as a bare id. `ItemArg` — the same type `/give` uses — accepts only a bare
//! id, for the same reason `/give`'s own doc gives: v1 ships with no textual
//! SNBT/tag grammar anywhere in this workspace. A single-item filter is exact
//! rather than a superset, which is the safe direction to be wrong in.

use lodestone_command::IntegerArgument;
use lodestone_command_mc::{EntityArg, ItemArg};

use super::registrar::{Ctx, Registrar};
use super::CommandResult;
use crate::commands::Effect;

/// `Commands.LEVEL_GAMEMASTERS`.
const CLEAR_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let clear = registrar.literal(root, "clear");
    registrar.require_level(clear, CLEAR_LEVEL);

    // Bare `/clear` — self, no filter, no cap. `@s`, built rather than parsed —
    // see `crate::commands::gamemode`'s own `self_selector` for why.
    registrar.exec(clear, |ctx| run(ctx, self_selector(), None, None));

    let (targets_node, targets_key) = registrar.arg(clear, "targets", EntityArg::players());
    registrar.exec(targets_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        run(ctx, selector, None, None)
    });

    let (item_node, item_key) = registrar.arg(targets_node, "item", ItemArg);
    registrar.exec(item_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let item = ctx.get(item_key).item.to_string();
        run(ctx, selector, Some(item), None)
    });

    let (count_node, count_key) =
        registrar.arg(item_node, "maxCount", IntegerArgument::bounded(0, i32::MAX));
    registrar.exec(count_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let item = ctx.get(item_key).item.to_string();
        let max_count = *ctx.get(count_key);
        run(ctx, selector, Some(item), Some(max_count))
    });
}

/// The `@s`-equivalent selector the no-target form resolves — matches
/// `crate::commands::gamemode`'s helper of the same shape.
fn self_selector() -> lodestone_command_mc::EntitySelector {
    lodestone_command_mc::EntitySelector {
        max_results: 1,
        includes_entities: true,
        current_entity: true,
        ..lodestone_command_mc::EntitySelector::default()
    }
}

fn run(
    ctx: &mut Ctx<'_>,
    selector: lodestone_command_mc::EntitySelector,
    item: Option<String>,
    max_count: Option<i32>,
) -> CommandResult {
    let targets = ctx.resolve(&selector)?;
    for target in &targets {
        ctx.effect(
            target.uuid,
            Effect::ClearInventory { item: item.clone(), max_count },
        );
    }
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("Cleared the inventory of {}", only.username));
    } else {
        ctx.send_success(format!("Cleared the inventory of {} players", targets.len()));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}
