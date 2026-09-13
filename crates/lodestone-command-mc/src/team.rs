//! `minecraft:team` and `minecraft:team_color` — `/team`'s own two argument
//! types. Membership (`<members>`) reuses
//! [`crate::ScoreHolderArg`] rather than a third type here: vanilla's own
//! team command registers `<members>` with the identical greedy
//! score-holder grammar `/scoreboard players`
//! already uses, so a selector or a bare "fake player" name works for team
//! membership exactly as it does for a score.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::text::TextColor;

use crate::McArg;

/// `minecraft:team` — a bare word naming a team. Not validated against a
/// live store here, for the identical reason [`crate::ObjectiveArg`] is not:
/// this crate has no world access at all (see the crate doc's "grammar here,
/// resolution there" split).
#[derive(Debug, Clone, Copy, Default)]
pub struct TeamArg;

impl ArgumentType for TeamArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let word = reader.read_unquoted_string();
        if word.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::InvalidInt(String::new())));
        }
        Ok(ParsedValue::dynamic(word))
    }
}

impl McArg for TeamArg {
    type Value = String;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Team
    }
}

/// `minecraft:team_color` — vanilla's `/team modify <team> color <value>`
/// reuses its own colour argument, whose value is one of its own
/// sixteen named legacy colours or `reset`.
///
/// `None` stands for `reset`: [`TextColor`] has no seventeenth variant for
/// it (the same modelling choice `lodestone_server::commands::team_store`
/// makes one layer up, on the storage side).
#[derive(Debug, Clone, Copy, Default)]
pub struct TeamColorArg;

impl ArgumentType for TeamColorArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let word = reader.read_unquoted_string();
        if word.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::InvalidInt(String::new())));
        }
        if word == "reset" {
            return Ok(ParsedValue::dynamic(None::<TextColor>));
        }
        let Some(color) = TextColor::from_name(&word) else {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::UnknownArgument));
        };
        Ok(ParsedValue::dynamic(Some(color)))
    }

    fn suggest(&self, partial: &str) -> Vec<String> {
        NAMED_ORDER
            .iter()
            .chain(std::iter::once(&"reset"))
            .filter(|name| name.starts_with(partial))
            .map(|name| (*name).to_string())
            .collect()
    }
}

/// The sixteen named colours in `§`-code order, for [`TeamColorArg::suggest`]
/// — [`TextColor`] does not expose its own name table publicly, so this is a
/// second, small, order-only list rather than a dependency on its private
/// layout.
const NAMED_ORDER: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

impl McArg for TeamColorArg {
    type Value = Option<TextColor>;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::TeamColor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_team(text: &str) -> String {
        let mut reader = StringReader::new(text);
        TeamArg.parse(&mut reader).unwrap().downcast_ref::<String>().unwrap().clone()
    }

    fn parse_color(text: &str) -> Option<TextColor> {
        let mut reader = StringReader::new(text);
        *TeamColorArg.parse(&mut reader).unwrap().downcast_ref::<Option<TextColor>>().unwrap()
    }

    #[test]
    fn a_bare_word_is_a_team_name() {
        assert_eq!(parse_team("red"), "red");
    }

    #[test]
    fn every_named_colour_and_reset_parse() {
        assert_eq!(parse_color("red"), Some(TextColor::Red));
        assert_eq!(parse_color("dark_purple"), Some(TextColor::DarkPurple));
        assert_eq!(parse_color("reset"), None);
    }

    #[test]
    fn an_unknown_colour_word_is_refused() {
        let mut reader = StringReader::new("not_a_colour");
        assert!(TeamColorArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identities_are_team_and_team_color() {
        assert_eq!(TeamArg.wire(), ArgumentParser::Team);
        assert_eq!(TeamColorArg.wire(), ArgumentParser::TeamColor);
    }
}
