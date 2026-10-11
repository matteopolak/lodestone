//! `/data get|merge|remove|modify` on `storage` and `entity` targets, plus
//! `/execute if`/`unless data storage` (in `crate::commands::execute`).
//!
//! # What it is
//!
//! One tree for two targets. A [`Target`] reads and writes a whole compound;
//! every verb works on that compound with [`lodestone_command_mc::NbtPath`], so
//! the grammar, the "found no elements" and "nothing changed" refusals, and the
//! result counts are the reference's for both. Feedback text is the reference
//! translations with the entity name standing in for the localised display name
//! (this crate carries no language table; a world entity prints its type id).
//!
//! # How it works
//!
//! - `storage` reads and writes `NbtStorageHandle`.
//! - `entity` reads `crate::commands::entity_data::target_nbt` and writes back
//!   through `MobSim::apply_entity_nbt`, the restore path a world load uses.
//!   Players are readable and never writable ("Unable to modify player data").
//!   A write that changes nothing is refused before the player check, as the
//!   reference orders them.
//! - `modify <path> (set|merge|insert <index>|prepend|append) value <nbt>`.
//!   The `from`, `string` and `compute` sources are not built.
//!
//! # How to change it
//!
//! A new target implements [`Target`]'s five methods and registers its literal
//! and argument in [`register_target`]. A new modify operation is one arm in
//! [`register_modify`].
//!
//! # Gaps
//!
//! No `block` target (see `crate::commands::entity_data`).

use std::sync::Arc;

use lodestone_command::{DoubleArgument, IntegerArgument, NodeId};
use lodestone_command_mc::{EntityArg, NbtCompoundArg, NbtPath, NbtPathArg, NbtTagArg, SnbtValue, StorageIdArg};

use super::entity_data::{nbt_to_snbt, resolve_single, snbt_to_nbt, target_nbt};
use super::registrar::{Ctx, Registrar};
use super::source::CommandTarget;

/// The game-masters permission level, matching `/scoreboard`/`/team`.
const DATA_LEVEL: u8 = 2;

const UNCHANGED: &str = "Nothing changed. The specified properties already have these values";
const TOO_DEEP: &str = "Tag is too deeply nested";

/// A resolved `/data` target.
enum Target {
    Storage(String),
    Entity(CommandTarget),
}

type Resolver = Arc<dyn Fn(&Ctx<'_>) -> Result<Target, String> + Send + Sync>;

impl Target {
    /// The target's whole compound.
    fn read(&self, ctx: &Ctx<'_>) -> Result<SnbtValue, String> {
        match self {
            Self::Storage(id) => Ok(ctx.world.state.nbt_storage().root(id)),
            Self::Entity(target) => target_nbt(ctx.world, target)
                .map(|nbt| nbt_to_snbt(&nbt))
                .ok_or_else(|| "No entity was found".to_owned()),
        }
    }

    /// Replaces the target's compound.
    fn write(&self, ctx: &Ctx<'_>, data: SnbtValue) -> Result<(), String> {
        let SnbtValue::Compound(entries) = data else { return Err(UNCHANGED.to_owned()) };
        match self {
            Self::Storage(id) => {
                ctx.world.state.nbt_storage().replace(id, entries);
                Ok(())
            }
            Self::Entity(CommandTarget::Player(_)) => Err("Unable to modify player data".to_owned()),
            Self::Entity(CommandTarget::World(entity)) => {
                let Some(mobs) = ctx.world.mobs else {
                    return Err("This command needs a live connection and is not available here".to_owned());
                };
                let nbt = snbt_to_nbt(&SnbtValue::Compound(entries));
                mobs.with(|sim| sim.apply_entity_nbt(entity.uuid, &nbt))
                    .map_err(|_| "Unable to modify entity data".to_owned())
            }
        }
    }

    fn modified(&self) -> String {
        match self {
            Self::Storage(id) => format!("Modified storage {id}"),
            Self::Entity(target) => format!("Modified entity data of {}", target.display_name()),
        }
    }

    fn query(&self, data: &SnbtValue) -> String {
        match self {
            Self::Storage(id) => format!("Storage {id} has the following contents: {data}"),
            Self::Entity(target) => format!("{} has the following entity data: {data}", target.display_name()),
        }
    }

    fn scaled(&self, path: &NbtPath, scale: f64, value: i32) -> String {
        match self {
            Self::Storage(id) => format!("{path} in storage {id} after scale factor of {scale:.2} is {value}"),
            Self::Entity(target) => {
                format!("{path} on {} after scale factor of {scale:.2} is {value}", target.display_name())
            }
        }
    }
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let data = registrar.literal(root, "data");
    registrar.require_level(data, DATA_LEVEL);

    for verb in ["merge", "get", "remove"] {
        let verb_node = registrar.literal(data, verb);
        for kind in ["entity", "storage"] {
            let (node, resolver) = register_target(registrar, verb_node, kind);
            match verb {
                "merge" => register_merge(registrar, node, &resolver),
                "get" => register_get(registrar, node, &resolver),
                _ => register_remove(registrar, node, &resolver),
            }
        }
    }
    let modify = registrar.literal(data, "modify");
    for kind in ["entity", "storage"] {
        let (node, resolver) = register_target(registrar, modify, kind);
        register_modify(registrar, node, &resolver);
    }
}

/// Registers `<kind> <target>` under `parent`; returns the target argument's
/// node and the resolver that turns its value into a [`Target`].
fn register_target(registrar: &mut Registrar, parent: NodeId, kind: &str) -> (NodeId, Resolver) {
    let literal = registrar.literal(parent, kind);
    if kind == "storage" {
        let (node, key) = registrar.arg(literal, "target", StorageIdArg);
        let resolver: Resolver = Arc::new(move |ctx| Ok(Target::Storage(ctx.get(key).clone())));
        (node, resolver)
    } else {
        let (node, key) = registrar.arg(literal, "target", EntityArg::entity());
        let resolver: Resolver = Arc::new(move |ctx| {
            let selector = ctx.get(key).clone();
            resolve_single(ctx, &selector).map(Target::Entity)
        });
        (node, resolver)
    }
}

fn register_get(registrar: &mut Registrar, node: NodeId, resolver: &Resolver) {
    let whole = resolver.clone();
    registrar.exec(node, move |ctx| {
        let target = whole(ctx)?;
        let data = target.read(ctx)?;
        ctx.send_success(target.query(&data));
        Ok(1)
    });
    let (path_node, path_key) = registrar.arg(node, "path", NbtPathArg);
    let by_path = resolver.clone();
    registrar.exec(path_node, move |ctx| {
        let target = by_path(ctx)?;
        let path = ctx.get(path_key).clone();
        let tag = single(&path, &target.read(ctx)?)?;
        let size = |len: usize| i32::try_from(len).unwrap_or(i32::MAX);
        let result = match &tag {
            SnbtValue::Compound(entries) => size(entries.len()),
            SnbtValue::List(items) => size(items.len()),
            SnbtValue::ByteArray(items) => size(items.len()),
            SnbtValue::IntArray(items) => size(items.len()),
            SnbtValue::LongArray(items) => size(items.len()),
            SnbtValue::String(text) => size(text.chars().count()),
            number => floor(numeric(number).unwrap_or(0.0)),
        };
        ctx.send_success(target.query(&tag));
        Ok(result)
    });
    let (scale_node, scale_key) = registrar.arg(path_node, "scale", DoubleArgument::new());
    let scaled = resolver.clone();
    registrar.exec(scale_node, move |ctx| {
        let target = scaled(ctx)?;
        let path = ctx.get(path_key).clone();
        let scale = *ctx.get(scale_key);
        let tag = single(&path, &target.read(ctx)?)?;
        let Some(value) = numeric(&tag) else {
            return Err(format!("Can't get {path}; only numeric tags are allowed"));
        };
        let result = floor(value * scale);
        ctx.send_success(target.scaled(&path, scale, result));
        Ok(result)
    });
}

/// The one value `path` selects.
fn single(path: &NbtPath, data: &SnbtValue) -> Result<SnbtValue, String> {
    let mut found = path.get(data)?;
    if found.len() > 1 {
        return Err("This argument accepts a single NBT value".to_owned());
    }
    Ok(found.pop().expect("a successful get is never empty"))
}

fn numeric(tag: &SnbtValue) -> Option<f64> {
    Some(match tag {
        SnbtValue::Byte(v) => f64::from(*v),
        SnbtValue::Short(v) => f64::from(*v),
        SnbtValue::Int(v) => f64::from(*v),
        SnbtValue::Long(v) => *v as f64,
        SnbtValue::Float(v) => f64::from(*v),
        SnbtValue::Double(v) => *v,
        _ => return None,
    })
}

#[allow(clippy::cast_possible_truncation)]
fn floor(value: f64) -> i32 {
    value.floor() as i32
}

fn register_merge(registrar: &mut Registrar, node: NodeId, resolver: &Resolver) {
    let (nbt_node, nbt_key) = registrar.arg(node, "nbt", NbtCompoundArg);
    let resolver = resolver.clone();
    registrar.exec(nbt_node, move |ctx| {
        let target = resolver(ctx)?;
        let incoming = ctx.get(nbt_key).clone();
        let old = target.read(ctx)?;
        let mut merged = old.clone();
        let SnbtValue::Compound(held) = &mut merged else { return Err(UNCHANGED.to_owned()) };
        lodestone_command_mc::merge_compound(held, &incoming);
        if old == merged {
            return Err(UNCHANGED.to_owned());
        }
        target.write(ctx, merged)?;
        ctx.send_success(target.modified());
        Ok(1)
    });
}

fn register_remove(registrar: &mut Registrar, node: NodeId, resolver: &Resolver) {
    let (path_node, path_key) = registrar.arg(node, "path", NbtPathArg);
    let resolver = resolver.clone();
    registrar.exec(path_node, move |ctx| {
        let target = resolver(ctx)?;
        let path = ctx.get(path_key).clone();
        let mut data = target.read(ctx)?;
        let removed = path.remove(&mut data);
        if removed == 0 {
            return Err(UNCHANGED.to_owned());
        }
        target.write(ctx, data)?;
        ctx.send_success(target.modified());
        Ok(i32::try_from(removed).unwrap_or(i32::MAX))
    });
}

/// How a `modify` operation changes the target compound; returns the count of
/// places changed.
type Operation = Arc<dyn Fn(&NbtPath, &mut SnbtValue, &SnbtValue) -> Result<usize, String> + Send + Sync>;

fn register_modify(registrar: &mut Registrar, node: NodeId, resolver: &Resolver) {
    let (path_node, path_key) = registrar.arg(node, "targetPath", NbtPathArg);

    let set: Operation = Arc::new(|path, data, value| path.set(data, value));
    let merge: Operation = Arc::new(|path, data, value| {
        if too_deep(value, 0) {
            return Err(TOO_DEEP.to_owned());
        }
        let SnbtValue::Compound(entries) = value else {
            return Err(format!("Expected an object: got {value}"));
        };
        path.merge(data, entries)
    });
    for (literal, operation) in [("set", set), ("merge", merge)] {
        let op_node = registrar.literal(path_node, literal);
        register_value(registrar, op_node, path_key, resolver, &operation);
    }

    for (literal, index) in [("prepend", Some(0)), ("append", Some(-1))] {
        let op_node = registrar.literal(path_node, literal);
        let operation: Operation =
            Arc::new(move |path, data, value| path.insert(index.unwrap_or(0), data, std::slice::from_ref(value)));
        register_value(registrar, op_node, path_key, resolver, &operation);
    }

    let insert = registrar.literal(path_node, "insert");
    let (index_node, index_key) = registrar.arg(insert, "index", IntegerArgument::new());
    register_value_with(registrar, index_node, path_key, resolver, move |ctx| {
        let index = *ctx.get(index_key);
        let operation: Operation = Arc::new(move |path, data, value| path.insert(index, data, std::slice::from_ref(value)));
        operation
    });
}

fn register_value(
    registrar: &mut Registrar,
    op_node: NodeId,
    path_key: super::registrar::ArgKey<NbtPath>,
    resolver: &Resolver,
    operation: &Operation,
) {
    let operation = operation.clone();
    register_value_with(registrar, op_node, path_key, resolver, move |_| operation.clone());
}

fn register_value_with(
    registrar: &mut Registrar,
    op_node: NodeId,
    path_key: super::registrar::ArgKey<NbtPath>,
    resolver: &Resolver,
    operation: impl Fn(&Ctx<'_>) -> Operation + Send + Sync + 'static,
) {
    let value_literal = registrar.literal(op_node, "value");
    let (value_node, value_key) = registrar.arg(value_literal, "value", NbtTagArg);
    let resolver = resolver.clone();
    registrar.exec(value_node, move |ctx| {
        let target = resolver(ctx)?;
        let path = ctx.get(path_key).clone();
        let value = ctx.get(value_key).clone();
        let operation = operation(ctx);
        let mut data = target.read(ctx)?;
        let changed = operation(&path, &mut data, &value)?;
        if changed == 0 {
            return Err(UNCHANGED.to_owned());
        }
        target.write(ctx, data)?;
        ctx.send_success(target.modified());
        Ok(i32::try_from(changed).unwrap_or(i32::MAX))
    });
}

fn too_deep(value: &SnbtValue, depth: usize) -> bool {
    if depth >= 512 {
        return true;
    }
    match value {
        SnbtValue::Compound(entries) => entries.iter().any(|(_, v)| too_deep(v, depth + 1)),
        SnbtValue::List(items) => items.iter().any(|v| too_deep(v, depth + 1)),
        _ => false,
    }
}
