//! Minecraft's own Brigadier argument types, and the one trait that ties a
//! parser to the wire node it is transmitted as.
//!
//! ## What it is
//!
//! [`lodestone_command`] is a faithful, dependency-free Brigadier port: nodes,
//! a `StringReader`, `parse`/`suggest`, and Brigadier's six *primitive*
//! argument types. It stops exactly where Minecraft begins, because an argument
//! type that knows what a game mode or an item is cannot live in a crate that
//! depends on nothing.
//!
//! This crate is that next layer: [`McArg`], plus [`GameModeArg`],
//! [`EntityArg`] (the `@p`/`@a`/`@e[…]` selector **grammar** and its AST),
//! [`Vec3Arg`]/[`BlockPosArg`], and [`ItemArg`].
//!
//! ## How it works — one call states both halves
//!
//! A command tree is transmitted to the client as well as executed on the
//! server, and the failure mode when those two disagree is specific and nasty:
//! the client autocompletes something the server then rejects. The defence is
//! [`McArg`], whose implementor supplies *both* the text parser
//! ([`lodestone_command::ArgumentType`], its supertrait) and the wire
//! descriptor ([`McArg::wire`]). A registrar therefore installs the parser and
//! records the wire identity in one call, and there is no second place where a
//! node's wire identity could be stated differently.
//!
//! [`McArg::wire`] returns [`lodestone_model::command_tree::ArgumentParser`] —
//! the *symbolic* version-free enum. The numeric
//! `minecraft:command_argument_type` registry ids it corresponds to live in the
//! version crates, and nothing here names one. That is what keeps this crate
//! (and the server, and therefore the shell) off the version seam.
//!
//! ## What "grammar here, resolution there" means, and why
//!
//! Vanilla splits an entity selector in exactly the same place. This crate
//! turns `@a[distance=..8,gamemode=creative]` into an [`EntitySelector`] — a
//! plain data AST with no world access at all — and the *server* resolves that
//! AST against its player registry. The split is not tidiness: resolution needs
//! a player list and a caller position, which is precisely the ECS-shaped
//! knowledge that must not enter this dependency graph, and the AST is what
//! lets a resolution test be written without a world.
//!
//! ## How to change it
//!
//! * **Adding an argument type:** implement [`lodestone_command::ArgumentType`]
//!   for the parse, then [`McArg`] for the wire. Get [`McArg::Value`] right —
//!   it must be the type your `parse` actually puts in the
//!   [`lodestone_command::ParsedValue`], because a typed-key API downcasts to
//!   it and a mismatch is a runtime panic at the first execution rather than a
//!   compile error. That is the single sharp edge in this design; see
//!   `lodestone_server::commands`' module doc for the full list of what is
//!   checked when.
//! * **Adding a selector option:** extend [`SelectorPredicate`] (or a scalar
//!   field on [`EntitySelector`]) and the `parse_option` match in
//!   [`entity`]. **The wire node does not change** — `minecraft:entity` carries
//!   only two flag bits and no option list — so a new option is invisible to
//!   the transmitted tree and cannot break parity with it.
//! * **Item components (`[…]` after the item id):** deliberately absent. See
//!   [`ItemArg`] for the exact refusal and why it is the honest layer for it.
//!
//! ## Dependencies
//!
//! [`lodestone_command`] (the tree and reader), [`lodestone_model`] (the
//! version-free `ArgumentParser`, `GameMode`, `ItemStack`), `lodestone-data`
//! (the item and entity-type name censuses), `uuid`.

pub mod anchor;
pub mod block;
pub mod dimension;
pub mod entity;
pub mod entity_type;
pub mod game_mode;
pub mod item;
pub mod position;
pub mod rotation;
pub mod scoreboard;
pub mod snbt;
pub mod swizzle;
pub mod team;
pub mod time;

pub use anchor::{AnchorInput, EntityAnchorArg};
pub use block::{BlockArg, BlockInput};
pub use dimension::{DimensionArg, HOSTED_DIMENSIONS};
pub use entity::{
    Bounds, EntityArg, EntitySelector, SelectorOrder, SelectorPosition, SelectorPredicate,
};
pub use entity_type::{EntityTypeArg, EntityTypeInput};
pub use game_mode::GameModeArg;
pub use item::{ItemArg, ItemInput};
pub use position::{BlockPosArg, Coordinate, Coordinates, Vec3Arg};
pub use rotation::{Rotation2, RotationArg};
pub use scoreboard::{
    IntRange, IntRangeArg, ObjectiveArg, ObjectiveCriteriaArg, OperationArg, ScoreHolderArg,
    ScoreHolderInput, ScoreOperation,
};
pub use snbt::{NbtCompoundArg, NbtTagArg, SnbtValue};
pub use swizzle::{Axes, SwizzleArg};
pub use team::{TeamArg, TeamColorArg};
pub use time::TimeArg;

use lodestone_command::{
    ArgumentType, BoolArgument, DoubleArgument, FloatArgument, IntegerArgument, LongArgument,
    StringArgument, StringKind as CmdStringKind,
};
use lodestone_model::command_tree::{ArgumentParser, StringKind as WireStringKind};
use lodestone_model::ids::ResourceKey;

/// An argument type that knows how it is transmitted as well as how it parses.
///
/// The supertrait bound is the point: an `McArg` **is** a
/// [`lodestone_command::ArgumentType`], so the value the server executes with
/// and the node the client is sent come from one object. A registrar that takes
/// `A: McArg` cannot install one without the other.
///
/// # `Value` is a promise this trait cannot check
///
/// [`Self::Value`] must be the Rust type your
/// [`ArgumentType::parse`](lodestone_command::ArgumentType::parse) actually
/// stores in the [`lodestone_command::ParsedValue`] it returns — `i32` for a
/// variant of `ParsedValue::Integer`, `GameMode` for a
/// `ParsedValue::dynamic(GameMode::…)`. Nothing in the type system relates the
/// two, because `parse` returns the erased enum. Get it wrong and the typed-key
/// downcast panics the first time that argument is *executed*, which one
/// execution test per command catches; there is no configuration in which it
/// fails silently.
pub trait McArg: ArgumentType + Sized + 'static {
    /// The Rust type this argument's value downcasts to.
    type Value: std::any::Any + Send + Sync + std::fmt::Debug;

    /// This node's wire identity — the `minecraft:command_argument_type` entry
    /// and its network payload, symbolically.
    fn wire(&self) -> ArgumentParser;

    /// The `FLAG_CUSTOM_SUGGESTIONS` provider id, if this node's completions
    /// have to be asked of the server rather than derived from the parser.
    ///
    /// `None` for everything in this crate today, and that is a measurement
    /// rather than a default: the captured 26.2 tree
    /// (`crates/protocol/v770/tests/fixtures/command_tree_creative.hex`) has no
    /// suggestion provider on any node of `/gamemode` or `/give` — vanilla
    /// leaves `minecraft:gamemode`, `minecraft:entity` and
    /// `minecraft:item_stack` to their own client-side `listSuggestions`.
    fn suggestion_provider(&self) -> Option<ResourceKey> {
        None
    }
}

/// `brigadier:integer`.
impl McArg for IntegerArgument {
    type Value = i32;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Integer { min: self.min, max: self.max }
    }
}

/// `brigadier:long`.
impl McArg for LongArgument {
    type Value = i64;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Long { min: self.min, max: self.max }
    }
}

/// `brigadier:float`.
///
/// Absent bounds are `±f32::MAX` on the wire, not `±inf`: `FloatArgumentInfo`
/// writes a flags byte plus only the bounds that are *present*, and the decode
/// substitutes `-f32::MAX`/`f32::MAX` for the missing ones. This crate's parser
/// defaults to `±inf`, so the two must be reconciled here rather than left to
/// look equivalent — an unbounded float projected as `inf` would not match a
/// captured vanilla node.
impl McArg for FloatArgument {
    type Value = f32;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Float {
            min: if self.min == f32::NEG_INFINITY { -f32::MAX } else { self.min },
            max: if self.max == f32::INFINITY { f32::MAX } else { self.max },
        }
    }
}

/// `brigadier:double`. Same `±MAX`-for-absent reconciliation as
/// [`FloatArgument`].
impl McArg for DoubleArgument {
    type Value = f64;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Double {
            min: if self.min == f64::NEG_INFINITY { -f64::MAX } else { self.min },
            max: if self.max == f64::INFINITY { f64::MAX } else { self.max },
        }
    }
}

/// `brigadier:bool`.
impl McArg for BoolArgument {
    type Value = bool;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Bool
    }
}

/// `brigadier:string`, carrying its `StringType` ordinal.
impl McArg for StringArgument {
    type Value = String;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::String(match self.kind {
            CmdStringKind::Word => WireStringKind::SingleWord,
            CmdStringKind::Quotable => WireStringKind::QuotablePhrase,
            CmdStringKind::Greedy => WireStringKind::GreedyPhrase,
        })
    }
}
