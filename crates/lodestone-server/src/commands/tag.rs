//! `/tag <targets> add|remove|list`.
//!
//! Tags live in the mob simulation's tag store, keyed by uuid, for every entity
//! including players; `tag=` in a selector reads the same store. A tag name is
//! one unquoted word, and an entity holds at most 1024.

use lodestone_command::StringArgument;
use lodestone_command_mc::EntityArg;

use super::registrar::{ArgKey, Ctx, Registrar};
use super::source::{CommandTarget, SelectorError, SelectorSubject};
use super::CommandResult;

/// The game-masters permission level.
const TAG_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let tag = registrar.literal(root, "tag");
    registrar.require_level(tag, TAG_LEVEL);
    let (targets_node, targets_key) = registrar.arg(tag, "targets", EntityArg::entities());

    for (literal, add) in [("add", true), ("remove", false)] {
        let node = registrar.literal(targets_node, literal);
        let (name_node, name_key) = registrar.arg(node, "name", StringArgument::word());
        registrar.exec(name_node, move |ctx| edit(ctx, targets_key, name_key, add));
    }

    let list = registrar.literal(targets_node, "list");
    registrar.exec(list, move |ctx| {
        let targets = resolve(ctx, targets_key)?;
        let table = ctx.world.mobs.map(|mobs| mobs.with(|sim| sim.tag_table())).unwrap_or_default();
        let mut all: Vec<String> = targets
            .iter()
            .flat_map(|t| table.get(&t.uuid()).cloned().unwrap_or_default())
            .collect();
        all.sort();
        all.dedup();
        let line = match (targets.as_slice(), all.is_empty()) {
            ([only], true) => format!("{} has no tags", only.display_name()),
            ([only], false) => format!("{} has {} tag(s): {}", only.display_name(), all.len(), all.join(", ")),
            (_, true) => format!("There are no tags on the {} entities", targets.len()),
            (_, false) => format!("The {} entities have {} total tag(s): {}", targets.len(), all.len(), all.join(", ")),
        };
        ctx.send_success(line);
        Ok(i32::try_from(all.len()).unwrap_or(i32::MAX))
    });
}

fn resolve(
    ctx: &Ctx<'_>,
    targets_key: ArgKey<lodestone_command_mc::EntitySelector>,
) -> Result<Vec<CommandTarget>, String> {
    let selector = ctx.get(targets_key).clone();
    ctx.resolve_targets(&selector).map_err(|error| match error {
        SelectorError::NoPlayersFound => "No entity was found".to_string(),
        other => other.to_string(),
    })
}

fn edit(
    ctx: &mut Ctx<'_>,
    targets_key: ArgKey<lodestone_command_mc::EntitySelector>,
    name_key: ArgKey<String>,
    add: bool,
) -> CommandResult {
    let targets = resolve(ctx, targets_key)?;
    let name = ctx.get(name_key).clone();
    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_string());
    };
    let players: Vec<uuid::Uuid> = ctx.world.players.iter().map(|p| p.uuid).collect();
    let changed = mobs.with(|sim| {
        sim.prune_entity_tags(&players);
        targets
            .iter()
            .filter(|t| if add { sim.add_entity_tag(t.uuid(), &name) } else { sim.remove_entity_tag(t.uuid(), &name) })
            .count()
    });
    if changed == 0 {
        return Err(if add {
            "Target either already has the tag or has too many tags"
        } else {
            "Target does not have this tag"
        }
        .to_string());
    }
    let (verb, prep) = if add { ("Added", "to") } else { ("Removed", "from") };
    match targets.as_slice() {
        [only] => ctx.send_success(format!("{verb} tag '{name}' {prep} {}", only.display_name())),
        _ => ctx.send_success(format!("{verb} tag '{name}' {prep} {} entities", targets.len())),
    }
    Ok(i32::try_from(changed).unwrap_or(i32::MAX))
}
