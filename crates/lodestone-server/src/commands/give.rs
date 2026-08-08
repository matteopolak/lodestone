//! `/give`, from `GiveCommand.java`.
//!
//! # The tree, as vanilla declares it
//!
//! ```text
//! literal("give").requires(LEVEL_GAMEMASTERS)
//!   └─ argument("targets", EntityArgument.players())
//!        └─ argument("item", ItemArgument.item(ctx))        [executable: count = 1]
//!             └─ argument("count", integer(1))              [executable]
//! ```
//!
//! Confirmed against the captured 26.2 tree (nodes 25 / 265 / 601 / 853): the
//! root literal and the `targets` node are **not** executable, `item` and `count`
//! both are, `item` is `minecraft:item_stack` with no payload, and `count` is
//! `brigadier:integer { min: 1, max: i32::MAX }`.
//!
//! Note the order: `<targets>` comes **before** `<item>`. That is the opposite of
//! how the command reads in English ("give a diamond to Steve") and is the shape
//! a from-memory reconstruction gets wrong.
//!
//! # `<targets>` is resolved once, then looped
//!
//! The handler receives the selector *value* and resolves it through `ctx` a
//! single time. Fork multiplicity (`execute as @a run give @s …`) is a separate
//! mechanism entirely — the dispatcher runs this whole handler once per forked
//! source — and conflating the two would double-apply on a forked path.

use lodestone_command::IntegerArgument;
use lodestone_command_mc::item::MAX_ALLOWED_ITEMSTACKS;
use lodestone_command_mc::{EntityArg, ItemArg, ItemInput};

use super::effect::Effect;
use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const GIVE_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let give = registrar.literal(root, "give");
    registrar.require_level(give, GIVE_LEVEL);

    let (targets_node, targets_key) = registrar.arg(give, "targets", EntityArg::players());
    let (item_node, item_key) = registrar.arg(targets_node, "item", ItemArg);

    // Two executable nodes on one path, one body. The default count is written
    // out at the shallow node — `giveItem(…, 1)` in vanilla — rather than being
    // an absent parameter, so the wire tree shows both nodes executable and
    // neither handler has to ask whether the other's argument was supplied.
    registrar.exec(item_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let item = ctx.get(item_key).clone();
        give_item(ctx, &selector, &item, 1)
    });

    let (count_node, count_key) = registrar.arg(item_node, "count", IntegerArgument::bounded(1, i32::MAX));
    registrar.exec(count_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let item = ctx.get(item_key).clone();
        let count = *ctx.get(count_key);
        give_item(ctx, &selector, &item, count)
    });
}

/// `GiveCommand.giveItem`.
fn give_item(
    ctx: &mut Ctx<'_>,
    selector: &lodestone_command_mc::EntitySelector,
    item: &ItemInput,
    count: i32,
) -> CommandResult {
    // `commands.give.failed.toomanyitems`: the cap is the item's *own* max stack
    // size times 100, not a flat number — 6400 diamonds and 100 diamond swords are
    // both exactly at the limit.
    let max_allowed = item.max_stack_size().saturating_mul(MAX_ALLOWED_ITEMSTACKS);
    let count = u32::try_from(count).map_err(|_| "The count must be positive".to_string())?;
    if count > max_allowed {
        return Err(format!(
            "You can't give more than {max_allowed} of {}",
            item.item
        ));
    }

    let targets = ctx.resolve(selector)?;
    let stacks = item.split_into_stacks(count);
    for target in &targets {
        ctx.effect(target.uuid, Effect::GiveItems(stacks.clone()));
    }

    // `commands.give.success.single` for one target, and the player-count form
    // for several.
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("Gave {count} {} to {}", item.item, only.username));
    } else {
        ctx.send_success(format!("Gave {count} {} to {} players", item.item, targets.len()));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}
