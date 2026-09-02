//! `minecraft:gamemode` — vanilla's own game-mode argument.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::GameMode;
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// The four game-mode names, in vanilla's own game-mode enum declaration
/// order — which is the order vanilla's own game-mode-argument suggestion
/// list offers them in, and the order the client shows them in.
pub const GAME_MODE_NAMES: [(&str, GameMode); 4] = [
    ("survival", GameMode::Survival),
    ("creative", GameMode::Creative),
    ("adventure", GameMode::Adventure),
    ("spectator", GameMode::Spectator),
];

/// Vanilla's own game-mode argument.
///
/// # 26.2 accepts the four full names and nothing else
///
/// Vanilla's own game-mode-argument parser reads an unquoted string and
/// looks it up by name, which delegates to a generic enum-name codec — an
/// exact match against each variant's serialized name. There are **no** `s`/`c`/`a`/`sp`
/// abbreviations and **no** numeric ids in 26.2; those were `/gamemode`'s
/// behaviour years ago and are gone. The hand-rolled `parse_gamemode_command`
/// this replaced accepted all eight, which is *more* permissive than vanilla
/// rather than less — a faithfulness bug that no test could have shown, because
/// it only ever made a command work that should have failed.
#[derive(Debug, Default, Clone, Copy)]
pub struct GameModeArg;

impl ArgumentType for GameModeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let text = reader.read_unquoted_string();
        match GAME_MODE_NAMES.iter().find(|(name, _)| *name == text) {
            Some((_, mode)) => Ok(ParsedValue::dynamic(*mode)),
            None => {
                reader.set_cursor(start);
                // `argument.gamemode.invalid` is a `DynamicCommandExceptionType`
                // over the offending text; `InvalidBool` is the closest shape
                // `lodestone-command`'s Brigadier-aligned `ParseErrorKind` has
                // for "found this, expected one of a closed set" — the same
                // reuse `ChoicesArgument` already makes, and deliberately not a
                // new variant, so this crate cannot grow its own error dialect.
                Err(ParseError::new(start, ParseErrorKind::InvalidBool(text)))
            }
        }
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        GAME_MODE_NAMES.iter().map(|(name, _)| (*name).to_string()).collect()
    }
}

impl McArg for GameModeArg {
    type Value = GameMode;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::GameMode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<GameMode, ParseError> {
        let mut reader = StringReader::new(text);
        GameModeArg
            .parse(&mut reader)
            .map(|value| *value.downcast_ref::<GameMode>().expect("GameModeArg produces a GameMode"))
    }

    /// The four names vanilla accepts, and the abbreviations it does **not**.
    ///
    /// The expected set comes from vanilla's own game-mode enum declaration plus
    /// its exact-match name codec, read this session — not from memory, which
    /// is exactly what got the abbreviations into the code this replaces.
    #[test]
    fn only_the_four_serialized_names_parse() {
        assert_eq!(parse("survival"), Ok(GameMode::Survival));
        assert_eq!(parse("creative"), Ok(GameMode::Creative));
        assert_eq!(parse("adventure"), Ok(GameMode::Adventure));
        assert_eq!(parse("spectator"), Ok(GameMode::Spectator));

        for rejected in ["s", "c", "a", "sp", "0", "1", "3", "Creative", "wizard", ""] {
            assert!(
                parse(rejected).is_err(),
                "26.2's game-mode name lookup rejects {rejected:?}; this accepted it"
            );
        }
    }

    /// A failed parse must leave the cursor where it started, or a sibling
    /// argument node tried after this one would see a half-consumed token.
    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("wizard hat");
        assert!(GameModeArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }

    #[test]
    fn suggestions_are_the_four_names_in_declaration_order() {
        assert_eq!(
            GameModeArg.suggest(""),
            ["survival", "creative", "adventure", "spectator"]
        );
    }
}
