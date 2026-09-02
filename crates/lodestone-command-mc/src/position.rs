//! `minecraft:vec3` and `minecraft:block_pos` — absolute, `~`-relative and
//! `^`-local coordinates.
//!
//! # Three coordinate dialects, not two
//!
//! * **absolute** — `12 64 -3`.
//! * **`~`-relative** — an *offset from the caller's position*, per component.
//!   `~` alone is `~0`.
//! * **`^`-local** — an offset in the caller's own facing frame (right, up,
//!   forward). Vanilla's own local-coordinates form is all-or-nothing: vanilla dispatches on the
//!   first character, so `^1 ~2 3` is a parse error rather than a mixture.
//!
//! `~` and `^` are **offsets, not positions**, and [`Coordinates::resolve`]
//! takes the caller's position (and, for the local dialect, its rotation) to turn
//! them into one. Nothing here reads a caller — the argument type produces the
//! offsets and the server resolves them, the same split
//! [`crate::entity`] uses.
//!
//! # The centre correction, which is easy to miss and visible in-game
//!
//! Vanilla's own vec3-argument parses `x` and `z` with centre-correction on
//! and `y`
//! **without** it. A centre-corrected absolute
//! component with no decimal point gains `+0.5`, so `/tp 10 64 10` puts you in
//! the middle of the column rather than on its corner — and `y` deliberately
//! does not, because the corner of a block *is* its floor. A port that applies
//! the correction uniformly leaves the player half a block inside the ceiling.
//! Vanilla's own block-pos argument never centre-corrects: its components are integers.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// One coordinate component: a value plus whether it is an offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinate {
    pub value: f64,
    /// `true` for `~`/`^` — the value is an offset from the caller.
    pub relative: bool,
}

impl Coordinate {
    /// An absolute component.
    #[must_use]
    pub const fn absolute(value: f64) -> Self {
        Self { value, relative: false }
    }

    /// An offset component.
    #[must_use]
    pub const fn relative(value: f64) -> Self {
        Self { value, relative: true }
    }

    /// This component resolved against the caller's own.
    #[must_use]
    pub fn resolve(self, origin: f64) -> f64 {
        if self.relative { origin + self.value } else { self.value }
    }
}

/// A parsed three-component position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    pub x: Coordinate,
    pub y: Coordinate,
    pub z: Coordinate,
    /// `true` when the three came from `^` — the components are (**left**, up,
    /// forward) in the caller's facing frame rather than in world axes.
    ///
    /// **Left, not right.** `LocalCoordinates` is
    /// `record LocalCoordinates(double left, double up, double forwards)` and
    /// its basis vector is `forwards.cross(up).scale(-1.0)`. A port that reads
    /// the first component as "right" is sign-flipped on one axis only, which
    /// looks plausible in every test that only moves forward.
    pub local: bool,
}

impl Coordinates {
    /// Resolve to a world position.
    ///
    /// `rotation` is `(yaw, pitch)` in degrees, and is only read for the local
    /// dialect. The basis is vanilla's own local-to-world coordinate
    /// rotation, transcribed directly: a forward vector from the
    /// yaw/pitch, an up vector from the same angles with pitch rotated 90°, and
    /// a **left** vector `forwards.cross(up).scale(-1.0)`.
    #[must_use]
    pub fn resolve(&self, origin: (f64, f64, f64), rotation: (f32, f32)) -> (f64, f64, f64) {
        if !self.local {
            return (
                self.x.resolve(origin.0),
                self.y.resolve(origin.1),
                self.z.resolve(origin.2),
            );
        }
        let (yaw, pitch) = (f64::from(rotation.0), f64::from(rotation.1));
        let yaw_rad = (yaw + 90.0).to_radians();
        let pitch_rad = (-pitch).to_radians();
        let up_pitch_rad = (-pitch + 90.0).to_radians();
        let (cos_yaw, sin_yaw) = (yaw_rad.cos(), yaw_rad.sin());
        let (cos_pitch, sin_pitch) = (pitch_rad.cos(), pitch_rad.sin());
        let (cos_up, sin_up) = (up_pitch_rad.cos(), up_pitch_rad.sin());
        let forward = (cos_yaw * cos_pitch, sin_pitch, sin_yaw * cos_pitch);
        let up = (cos_yaw * cos_up, sin_up, sin_yaw * cos_up);
        // left = forwards × up, scaled by -1 (vanilla's own basis construction).
        let left = (
            -(forward.1 * up.2 - forward.2 * up.1),
            -(forward.2 * up.0 - forward.0 * up.2),
            -(forward.0 * up.1 - forward.1 * up.0),
        );
        (
            origin.0 + left.0 * self.x.value + up.0 * self.y.value + forward.0 * self.z.value,
            origin.1 + left.1 * self.x.value + up.1 * self.y.value + forward.1 * self.z.value,
            origin.2 + left.2 * self.x.value + up.2 * self.y.value + forward.2 * self.z.value,
        )
    }
}

/// Vanilla's own vec3 argument — `minecraft:vec3`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vec3Arg {
    /// Whether an absolute `x`/`z` with no decimal point gains `+0.5`.
    /// Vanilla's own default constructor sets this `true`; its exact-form
    /// constructor is the opt-out
    /// (`/spreadplayers`, `/worldborder center`).
    pub centre_correct: bool,
}

impl Vec3Arg {
    /// Vanilla's own default vec3-argument constructor.
    #[must_use]
    pub const fn new() -> Self {
        Self { centre_correct: true }
    }

    /// Vanilla's own exact-form vec3-argument constructor.
    #[must_use]
    pub const fn exact() -> Self {
        Self { centre_correct: false }
    }
}

impl ArgumentType for Vec3Arg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = if reader.peek() == Some('^') {
            read_local(reader)
        } else {
            read_world_double(reader, self.centre_correct)
        };
        match result {
            Ok(coordinates) => Ok(ParsedValue::dynamic(coordinates)),
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

impl McArg for Vec3Arg {
    type Value = Coordinates;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Vec3
    }
}

/// Vanilla's own block-pos argument — `minecraft:block_pos`.
///
/// Integer components, never centre-corrected. `~`-relative components are still
/// legal and are still offsets; `^`-local is legal too and resolves through the
/// same basis before flooring.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockPosArg;

impl ArgumentType for BlockPosArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let result = if reader.peek() == Some('^') {
            read_local(reader)
        } else {
            read_world_int(reader)
        };
        match result {
            Ok(coordinates) => Ok(ParsedValue::dynamic(coordinates)),
            Err(e) => {
                reader.set_cursor(start);
                Err(e)
            }
        }
    }
}

impl McArg for BlockPosArg {
    type Value = Coordinates;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::BlockPos
    }
}

/// Vanilla's own world-coordinates double reader — note the centre
/// correction applies to `x`
/// and `z` only.
fn read_world_double(reader: &mut StringReader, centre_correct: bool) -> Result<Coordinates, ParseError> {
    let x = read_double_component(reader, centre_correct)?;
    expect_separator(reader)?;
    let y = read_double_component(reader, false)?;
    expect_separator(reader)?;
    let z = read_double_component(reader, centre_correct)?;
    Ok(Coordinates { x, y, z, local: false })
}

/// Vanilla's own world-coordinates int reader.
fn read_world_int(reader: &mut StringReader) -> Result<Coordinates, ParseError> {
    let x = read_int_component(reader)?;
    expect_separator(reader)?;
    let y = read_int_component(reader)?;
    expect_separator(reader)?;
    let z = read_int_component(reader)?;
    Ok(Coordinates { x, y, z, local: false })
}

/// Vanilla's own local-coordinates reader — all three components must carry `^`.
fn read_local(reader: &mut StringReader) -> Result<Coordinates, ParseError> {
    let x = read_local_component(reader)?;
    expect_separator(reader)?;
    let y = read_local_component(reader)?;
    expect_separator(reader)?;
    let z = read_local_component(reader)?;
    Ok(Coordinates { x, y, z, local: true })
}

/// Vanilla's own local-coordinates double reader.
fn read_local_component(reader: &mut StringReader) -> Result<Coordinate, ParseError> {
    let position = reader.cursor();
    if !reader.can_read() {
        return Err(ParseError::new(position, ParseErrorKind::ExpectedDouble));
    }
    if reader.peek() != Some('^') {
        return Err(mixed_type(position));
    }
    reader.skip();
    let value = if has_value_here(reader) { reader.read_double()? } else { 0.0 };
    Ok(Coordinate::relative(value))
}

/// Vanilla's own world-coordinate double reader.
fn read_double_component(reader: &mut StringReader, centre_correct: bool) -> Result<Coordinate, ParseError> {
    let position = reader.cursor();
    if reader.peek() == Some('^') {
        return Err(mixed_type(position));
    }
    if !reader.can_read() {
        return Err(ParseError::new(position, ParseErrorKind::ExpectedDouble));
    }
    let relative = reader.peek() == Some('~');
    if relative {
        reader.skip();
    }
    let start = reader.cursor();
    let value = if has_value_here(reader) { reader.read_double()? } else { 0.0 };
    // Vanilla's own world-coordinate double reader's own test is on the *text*: a component
    // written without a decimal point is a block reference and gains 0.5.
    // Testing the parsed value instead would centre-correct `10.0`, which
    // vanilla does not.
    let text: String = reader.source().chars().skip(start).take(reader.cursor() - start).collect();
    if relative {
        return Ok(Coordinate::relative(if text.is_empty() { 0.0 } else { value }));
    }
    let corrected = if centre_correct && !text.contains('.') { value + 0.5 } else { value };
    Ok(Coordinate::absolute(corrected))
}

/// Vanilla's own world-coordinate int reader.
///
/// The asymmetry is vanilla's and is easy to miss: an **absolute** component is
/// read as an int, but a **relative** one is read as a double, so `/setblock ~1.5 ~ ~`
/// really is legal and `/setblock 1.5 64 0` really is not.
fn read_int_component(reader: &mut StringReader) -> Result<Coordinate, ParseError> {
    let position = reader.cursor();
    if reader.peek() == Some('^') {
        return Err(mixed_type(position));
    }
    if !reader.can_read() {
        return Err(ParseError::new(position, ParseErrorKind::ExpectedInt));
    }
    let relative = reader.peek() == Some('~');
    if relative {
        reader.skip();
    }
    let value = if has_value_here(reader) {
        if relative { reader.read_double()? } else { f64::from(reader.read_int()?) }
    } else {
        0.0
    };
    Ok(if relative { Coordinate::relative(value) } else { Coordinate::absolute(value) })
}

/// Vanilla's own test for "is there
/// a number here", which is *not* "does a digit follow". `~` at end of input or
/// before a space is `~0`; `~x` is a double-read that fails, and must, rather
/// than silently becoming `~0` and desyncing the remaining components.
fn has_value_here(reader: &StringReader) -> bool {
    reader.can_read() && reader.peek() != Some(' ')
}

/// Vanilla's own mixed-coordinate-type error.
fn mixed_type(position: usize) -> ParseError {
    ParseError::new(
        position,
        ParseErrorKind::InvalidDouble("cannot mix world and local coordinates".to_string()),
    )
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

    fn vec3(text: &str) -> Coordinates {
        let mut reader = StringReader::new(text);
        let value = Vec3Arg::new().parse(&mut reader).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        *value.downcast_ref::<Coordinates>().expect("Vec3Arg produces Coordinates")
    }

    fn block_pos(text: &str) -> Coordinates {
        let mut reader = StringReader::new(text);
        let value = BlockPosArg.parse(&mut reader).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        *value.downcast_ref::<Coordinates>().expect("BlockPosArg produces Coordinates")
    }

    /// The centre correction, with both hypotheses computed from outside: the
    /// vanilla rule (`x`/`z` corrected, `y` not) and the plausible-wrong one
    /// (all three corrected). The assertion lands on exactly one.
    #[test]
    fn vec3_centre_corrects_x_and_z_but_not_y() {
        let position = vec3("10 64 -3");
        assert_eq!(position.x, Coordinate::absolute(10.5));
        assert_eq!(position.y, Coordinate::absolute(64.0), "y is never centre-corrected");
        assert_eq!(position.z, Coordinate::absolute(-2.5));

        // A component *written* with a decimal point is taken as given, even
        // when its value is integral — the test is on the text, not the value.
        assert_eq!(vec3("10.0 64 -3.0").x, Coordinate::absolute(10.0));

        // The opt-out corrects nothing.
        let mut reader = StringReader::new("10 64 -3");
        let exact = Vec3Arg::exact().parse(&mut reader).expect("parses");
        let exact = exact.downcast_ref::<Coordinates>().expect("Coordinates");
        assert_eq!(exact.x, Coordinate::absolute(10.0));
        assert_eq!(exact.z, Coordinate::absolute(-3.0));
    }

    #[test]
    fn tilde_is_an_offset_and_a_bare_tilde_is_zero() {
        let position = vec3("~ ~2 ~-0.5");
        assert_eq!(position.x, Coordinate::relative(0.0));
        assert_eq!(position.y, Coordinate::relative(2.0));
        assert_eq!(position.z, Coordinate::relative(-0.5));
        assert!(!position.local);

        // Resolution is against the caller, and a relative component is never
        // centre-corrected.
        assert_eq!(position.resolve((100.0, 64.0, -8.0), (0.0, 0.0)), (100.0, 66.0, -8.5));
    }

    /// `^` is all-or-nothing: a mixture is a parse error, not a silent
    /// reinterpretation of the remaining components.
    #[test]
    fn local_coordinates_are_all_or_nothing() {
        let local = vec3("^1 ^2 ^3");
        assert!(local.local);
        assert_eq!(local.x, Coordinate::relative(1.0));

        let mut reader = StringReader::new("^1 ~2 3");
        assert!(Vec3Arg::new().parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");

        let mut reader = StringReader::new("1 ^2 3");
        assert!(Vec3Arg::new().parse(&mut reader).is_err());
    }

    /// The local basis, checked at a rotation where the answer is exact.
    ///
    /// Minecraft's yaw 0 faces +Z ("south"), so `^0 ^0 ^5` must be five blocks in
    /// +Z. The first component is **left** — `LocalCoordinates(left, up,
    /// forwards)` — and left of +Z is +X, which is the *opposite* of what a
    /// reading of it as "right" predicts. Both hypotheses are computed here and
    /// the assertion lands on exactly one; the first run of this test landed on
    /// the wrong one and the jar record settled it.
    #[test]
    fn the_local_basis_is_left_up_forwards_not_right_up_forwards() {
        let forward = vec3("^0 ^0 ^5").resolve((0.0, 0.0, 0.0), (0.0, 0.0));
        assert!(forward.0.abs() < 1e-9, "forward has no x at yaw 0: {forward:?}");
        assert!(forward.1.abs() < 1e-9, "forward has no y at pitch 0: {forward:?}");
        assert!((forward.2 - 5.0).abs() < 1e-9, "yaw 0 faces +Z: {forward:?}");

        let sideways = vec3("^5 ^0 ^0").resolve((0.0, 0.0, 0.0), (0.0, 0.0));
        assert!(
            (sideways.0 - 5.0).abs() < 1e-9,
            "the first component is `left`, and left of +Z is +X (a `right` reading predicts -5): {sideways:?}"
        );
        assert!(sideways.2.abs() < 1e-9, "{sideways:?}");

        let up = vec3("^0 ^5 ^0").resolve((0.0, 0.0, 0.0), (0.0, 0.0));
        assert!((up.1 - 5.0).abs() < 1e-9, "up is +Y at pitch 0: {up:?}");

        // A second, independent rotation, so the +X above cannot be a
        // coincidence of the identity case. Yaw -90 faces +X (east); facing east
        // with +Y up, north (-Z) is on your left, so `^5` is -5 in Z.
        let sideways = vec3("^5 ^0 ^0").resolve((0.0, 0.0, 0.0), (-90.0, 0.0));
        assert!((sideways.2 + 5.0).abs() < 1e-9, "left of +X is -Z: {sideways:?}");
        assert!(sideways.0.abs() < 1e-9, "{sideways:?}");
    }

    /// A relative block-pos component reads a **double** while an absolute one
    /// reads an int (vanilla's own world-coordinate int reader's own asymmetry).
    #[test]
    fn a_relative_block_pos_component_may_be_fractional_but_an_absolute_one_may_not() {
        assert_eq!(block_pos("~1.5 ~ ~").x, Coordinate::relative(1.5));
        let mut reader = StringReader::new("1.5 64 0");
        assert!(BlockPosArg.parse(&mut reader).is_err());
    }

    #[test]
    fn block_pos_takes_integers_and_never_centre_corrects() {
        let position = block_pos("10 64 -3");
        assert_eq!(position.x, Coordinate::absolute(10.0));
        assert_eq!(position.y, Coordinate::absolute(64.0));
        assert_eq!(position.z, Coordinate::absolute(-3.0));

        assert_eq!(block_pos("~ ~-1 ~").y, Coordinate::relative(-1.0));

        // A fractional component is not an integer.
        let mut reader = StringReader::new("10.5 64 -3");
        assert!(BlockPosArg.parse(&mut reader).is_err());
    }

    #[test]
    fn the_wire_identities_are_the_two_no_payload_parsers() {
        assert_eq!(Vec3Arg::new().wire(), ArgumentParser::Vec3);
        assert_eq!(BlockPosArg.wire(), ArgumentParser::BlockPos);
    }
}
