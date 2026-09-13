//! `ParsedValue::Dyn` — the structured-payload variant a Minecraft-flavoured
//! argument type produces, and the uniform downcast that reads it back.
//!
//! The load-bearing property is not "an `Arc<dyn Any>` round-trips" (that is
//! `std`'s job) but that [`ParsedValue::downcast_ref`] is uniform across the
//! *primitive* variants too. That uniformity is what lets a typed argument-key
//! API — `lodestone_server::commands::ArgKey<T>` — have exactly one extraction
//! path for `ArgKey<i32>` and `ArgKey<EntitySelector>` alike. A `Dyn`-only
//! downcast would have compiled and forced a second path for every primitive.

use std::sync::Arc;

use lodestone_command::{ArgumentType, CommandTree, ParseError, ParsedValue, StringReader};

/// A structured payload of the shape `lodestone-command-mc` actually produces:
/// not a newtype over a primitive, so a downcast that accidentally matched the
/// primitive variant could not pass.
#[derive(Debug, Clone, PartialEq)]
struct Selector {
    kind: char,
    limit: usize,
}

/// An argument type whose result is [`Selector`], parsed `@p`-style.
#[derive(Debug)]
struct SelectorArgument;

impl ArgumentType for SelectorArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        // `read_unquoted_string` would consume nothing here: `@` is not in
        // `StringReader::is_allowed_in_unquoted_string`'s `[0-9A-Za-z_.+-]`,
        // which is exactly why vanilla's own `EntitySelectorParser` reads the
        // `@` with a bare `read()` before dispatching on the kind character.
        let kind = match (reader.read(), reader.read()) {
            (Some('@'), Some(kind)) => kind,
            _ => '?',
        };
        Ok(ParsedValue::dynamic(Selector {
            kind,
            limit: if kind == 'a' { usize::MAX } else { 1 },
        }))
    }
}

#[test]
fn a_dyn_value_survives_a_real_parse_and_downcasts_to_its_own_type() {
    let mut tree = CommandTree::new();
    let root = tree.root();
    let literal = tree.add_literal(root, "gamemode");
    let mode = tree.add_argument(literal, "mode", Arc::new(lodestone_command::StringArgument::word()));
    let target = tree.add_argument(mode, "target", Arc::new(SelectorArgument));
    tree.set_executable(target, true);

    let parsed = tree.parse("gamemode creative @a").expect("must parse");

    // The primitive slot and the structured slot come out of the same API.
    let mode_value: &String = parsed
        .argument("mode")
        .expect("mode slot")
        .downcast_ref()
        .expect("a String argument downcasts to String");
    assert_eq!(mode_value, "creative");

    let selector: &Selector = parsed
        .argument("target")
        .expect("target slot")
        .downcast_ref()
        .expect("a Dyn argument downcasts to its own type");
    assert_eq!(selector, &Selector { kind: 'a', limit: usize::MAX });
}

/// The negative half: a downcast to the *wrong* type answers `None` rather
/// than a plausible-looking value. Without this, `downcast_ref` returning
/// `Some` for everything would satisfy the test above.
#[test]
fn a_downcast_to_the_wrong_type_answers_none_for_every_variant() {
    assert_eq!(ParsedValue::Integer(7).downcast_ref::<i32>(), Some(&7));
    assert_eq!(ParsedValue::Integer(7).downcast_ref::<i64>(), None);
    assert_eq!(ParsedValue::Long(7).downcast_ref::<i64>(), Some(&7));
    assert_eq!(ParsedValue::Bool(true).downcast_ref::<bool>(), Some(&true));
    assert_eq!(ParsedValue::Bool(true).downcast_ref::<i32>(), None);
    assert_eq!(
        ParsedValue::Custom("x".to_string()).downcast_ref::<String>(),
        Some(&"x".to_string())
    );

    let dynamic = ParsedValue::dynamic(Selector { kind: 's', limit: 1 });
    assert!(dynamic.downcast_ref::<Selector>().is_some());
    assert!(dynamic.downcast_ref::<String>().is_none());
    assert!(dynamic.downcast_ref::<i32>().is_none());
}

/// `Dyn` equality is pointer equality, stated as a test so nobody later reads
/// `PartialEq` on a `ParsedValue` as a structural comparison of its payload.
#[test]
fn two_structurally_equal_dyn_values_are_not_equal_unless_they_are_the_same_allocation() {
    let one = ParsedValue::dynamic(Selector { kind: 'p', limit: 1 });
    let same = one.clone();
    let equal_but_separate = ParsedValue::dynamic(Selector { kind: 'p', limit: 1 });

    assert_eq!(one, same, "a clone shares the allocation");
    assert_ne!(one, equal_but_separate, "a separate allocation is not equal");
    // The control that this is about `Dyn` and not about `PartialEq` being
    // broken outright.
    assert_eq!(ParsedValue::Integer(1), ParsedValue::Integer(1));
    assert_ne!(ParsedValue::Integer(1), ParsedValue::Long(1));
}
