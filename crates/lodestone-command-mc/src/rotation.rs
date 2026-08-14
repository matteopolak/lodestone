//! `minecraft:rotation` — `RotationArgument.rotation()`, `/execute rotated
//! <rotation>`'s two-component `<yaw> <pitch>`.
//!
//! # The field names in the jar do not match the read order, and that is not a
//! transposition bug
//!
//! `RotationArgument.parse` reads its **first** token into a local named `y`
//! and its **second** into a local named `x`, then builds
//! `new WorldCoordinates(x, y, zero)`. That looks backwards until
//! `WorldCoordinates.getRotation` is read too: it returns
//! `Vec2(x.get(rot.x), y.get(rot.y))`, and vanilla's own `Vec2` rotation
//! convention is `(x = pitch, y = yaw)` (`Entity.getRotationVector()`). So the
//! local named `x` (the **second**-read token) ends up controlling the output
//! *pitch*, and the local named `y` (the **first**-read token) ends up
//! controlling the output *yaw* — the locals are named for the `WorldCoordinates`
//! field they occupy, not for the order they were read in. Net effect: input
//! order really is `<yaw> <pitch>`, matching every in-game usage
//! (`/execute rotated 0 0`, `TeleportCommand`'s own `<rotation>`), and this
//! module reads `yaw` first and `pitch` second directly — no transposition,
//! just confusingly-named upstream locals that this port does not reproduce.
//!
//! # `~`-relative, never `^`-local
//!
//! Each component is `WorldCoordinate.parseDouble(reader, false)` — the same
//! grammar [`crate::position::Vec3Arg`]'s absolute/`~` components use (no
//! centre correction; a bare `~` is `~0`), but rotation has no `^` dialect at
//! all, so a leading `^` is simply not `~` and parses as (or fails as) an
//! absolute number like any other unexpected character would.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::position::Coordinate;
use crate::McArg;

/// A parsed `<yaw> <pitch>` pair, each possibly `~`-relative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation2 {
    pub yaw: Coordinate,
    pub pitch: Coordinate,
}

impl Rotation2 {
    /// Resolve against the command source's own current rotation —
    /// `WorldCoordinates.getRotation(source)`.
    #[must_use]
    pub fn resolve(&self, source_rotation: (f32, f32)) -> (f32, f32) {
        let yaw = self.yaw.resolve(f64::from(source_rotation.0));
        #[allow(clippy::cast_possible_truncation)]
        let yaw = yaw as f32;
        let pitch = self.pitch.resolve(f64::from(source_rotation.1));
        #[allow(clippy::cast_possible_truncation)]
        let pitch = pitch as f32;
        (yaw, pitch)
    }
}

/// `RotationArgument.rotation()` — `minecraft:rotation`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RotationArg;

impl ArgumentType for RotationArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = (|| -> Result<Rotation2, ParseError> {
            let yaw = read_component(reader)?;
            expect_separator(reader)?;
            let pitch = read_component(reader)?;
            Ok(Rotation2 { yaw, pitch })
        })();
        match result {
            Ok(value) => Ok(ParsedValue::dynamic(value)),
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

impl McArg for RotationArg {
    type Value = Rotation2;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Rotation
    }
}

/// `WorldCoordinate.parseDouble(reader, false)` — never centre-corrected, no
/// `^` dialect (a leading `^` here is just not a `~` and falls through to a
/// plain, and here invalid, number read).
fn read_component(reader: &mut StringReader) -> Result<Coordinate, ParseError> {
    let position = reader.cursor();
    if !reader.can_read() {
        return Err(ParseError::new(position, ParseErrorKind::ExpectedDouble));
    }
    let relative = reader.peek() == Some('~');
    if relative {
        reader.skip();
    }
    let value = if has_value_here(reader) { reader.read_double()? } else { 0.0 };
    Ok(if relative { Coordinate::relative(value) } else { Coordinate::absolute(value) })
}

fn has_value_here(reader: &StringReader) -> bool {
    reader.can_read() && reader.peek() != Some(' ')
}

fn expect_separator(reader: &mut StringReader) -> Result<(), ParseError> {
    if reader.peek() == Some(' ') {
        reader.skip();
        Ok(())
    } else {
        Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot(text: &str) -> Rotation2 {
        let mut reader = StringReader::new(text);
        let value = RotationArg.parse(&mut reader).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        *value.downcast_ref::<Rotation2>().expect("RotationArg produces Rotation2")
    }

    /// Input order is `<yaw> <pitch>`, pairwise-distinct so a transposition
    /// cannot pass by coincidence.
    #[test]
    fn reads_yaw_then_pitch_in_that_order() {
        let r = rot("11 4");
        assert_eq!(r.yaw, Coordinate::absolute(11.0));
        assert_eq!(r.pitch, Coordinate::absolute(4.0));
        assert_eq!(r.resolve((0.0, 0.0)), (11.0, 4.0));
    }

    #[test]
    fn both_components_may_be_tilde_relative() {
        let r = rot("~-5 ~5");
        assert_eq!(r.yaw, Coordinate::relative(-5.0));
        assert_eq!(r.pitch, Coordinate::relative(5.0));
        assert_eq!(r.resolve((90.0, 10.0)), (85.0, 15.0));
    }

    #[test]
    fn a_bare_tilde_is_a_zero_offset() {
        let r = rot("~ ~");
        assert_eq!(r.resolve((45.0, -20.0)), (45.0, -20.0));
    }

    #[test]
    fn the_wire_identity_is_the_no_payload_rotation_parser() {
        assert_eq!(RotationArg.wire(), ArgumentParser::Rotation);
    }
}
