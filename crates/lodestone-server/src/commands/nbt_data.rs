//! `/data storage` (the consumer `crate::commands::nbt_storage` was built
//! for) plus `/execute if`/`unless data storage`, its dedicated conditional
//! in `crate::commands::execute`.
//!
//! # Only `storage`, not `entity`/`block`
//!
//! The real `/data` has three targets. This crate builds one — see
//! `crate::commands::nbt_storage`'s module doc for why `entity`/`block` are
//! a different, still-missing subsystem (a live, command-reachable, mutable
//! NBT view of an entity or block entity) rather than an oversight here.

use lodestone_command_mc::{NbtCompoundArg, NbtPathArg, SnbtValue, StorageIdArg};

use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// The game-masters permission level, matching `/scoreboard`/`/team`.
const DATA_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let data = registrar.literal(root, "data");
    registrar.require_level(data, DATA_LEVEL);

    register_get(registrar, data);
    register_merge(registrar, data);
    register_remove(registrar, data);
}

fn register_get(registrar: &mut Registrar, data: lodestone_command::NodeId) {
    let get = registrar.literal(data, "get");
    let storage = registrar.literal(get, "storage");
    let (id_node, id_key) = registrar.arg(storage, "target", StorageIdArg);

    // `data get storage <id>` — the whole compound.
    registrar.exec(id_node, move |ctx| {
        let id = ctx.get(id_key).clone();
        let value = ctx.world.state.nbt_storage().get(&id, &[]).unwrap_or(SnbtValue::Compound(Vec::new()));
        report_get(ctx, &id, &value)
    });

    let (path_node, path_key) = registrar.arg(id_node, "path", NbtPathArg);
    registrar.exec(path_node, move |ctx| {
        let id = ctx.get(id_key).clone();
        let path = ctx.get(path_key).clone();
        match ctx.world.state.nbt_storage().get(&id, &path) {
            Some(value) => report_get(ctx, &id, &value),
            None => Err(format!("Found no elements matching {}", path.join("."))),
        }
    });
}

fn report_get(ctx: &mut Ctx<'_>, id: &str, value: &SnbtValue) -> CommandResult {
    ctx.send_success(format!("Storage {id} has the following contents: {value}"));
    Ok(1)
}

fn register_merge(registrar: &mut Registrar, data: lodestone_command::NodeId) {
    let merge = registrar.literal(data, "merge");
    let storage = registrar.literal(merge, "storage");
    let (id_node, id_key) = registrar.arg(storage, "target", StorageIdArg);
    let (nbt_node, nbt_key) = registrar.arg(id_node, "nbt", NbtCompoundArg);
    registrar.exec(nbt_node, move |ctx| {
        let id = ctx.get(id_key).clone();
        let nbt = ctx.get(nbt_key).clone();
        ctx.world.state.nbt_storage().merge(&id, nbt);
        ctx.send_success(format!("Updated storage {id}"));
        Ok(1)
    });
}

fn register_remove(registrar: &mut Registrar, data: lodestone_command::NodeId) {
    let remove = registrar.literal(data, "remove");
    let storage = registrar.literal(remove, "storage");
    let (id_node, id_key) = registrar.arg(storage, "target", StorageIdArg);
    let (path_node, path_key) = registrar.arg(id_node, "path", NbtPathArg);
    registrar.exec(path_node, move |ctx| {
        let id = ctx.get(id_key).clone();
        let path = ctx.get(path_key).clone();
        if ctx.world.state.nbt_storage().remove(&id, &path) {
            ctx.send_success(format!("Removed element from storage {id}"));
            Ok(1)
        } else {
            Err(format!("Found no elements matching {}", path.join(".")))
        }
    });
}
