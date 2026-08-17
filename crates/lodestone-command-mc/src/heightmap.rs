//! `minecraft:heightmap` — `HeightmapTypeArgument.heightmap()`, `/execute
//! positioned over <heightmap>` (issue #48's remainder).
//!
//! # Four names, not six
//!
//! `Heightmap.Types` has six variants, but `HeightmapTypeArgument` filters to
//! `Heightmap.Types::keepAfterWorldgen` — the two `_WG` (worldgen-only)
//! variants are dropped from the argument's own choice set, because they
//! exist only to seed structure placement mid-generation and are discarded
//! once a chunk finishes. [`HEIGHTMAP_NAMES`] lists exactly the four survivors,
//! lower-cased (`HeightmapTypeArgument.convertId`), in `Heightmap.Types`'
//! declaration order.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// One of the four heightmap types `/execute positioned over` can name —
/// `Heightmap.Types::keepAfterWorldgen`'s own predicate, evaluated at the
/// call site (`lodestone_server::commands::execute`) against a per-cell
/// block-state scan rather than a stored per-column table: only
/// `MOTION_BLOCKING` is ever cached on a generated column, and even that only
/// for a column fresh off the generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightmapKind {
    /// `Heightmap.NOT_AIR` — the highest non-air cell, any material.
    WorldSurface,
    /// `Heightmap.MATERIAL_MOTION_BLOCKING` — the highest cell that
    /// `blocksMotion()`, excluding fluids (unlike `MotionBlocking` below).
    OceanFloor,
    /// `state.blocksMotion() || !state.getFluidState().isEmpty()` — the
    /// highest solid-or-fluid cell. The one vanilla's own F3 debug screen and
    /// `/execute positioned over motion_blocking` (its most common use) both
    /// read.
    MotionBlocking,
    /// [`Self::MotionBlocking`], with every leaf block excluded from the
    /// predicate — vanilla's own use for mob-spawn placement, so a leaf
    /// canopy does not count as ground.
    MotionBlockingNoLeaves,
}

/// The four surviving names, in `Heightmap.Types`' own declaration order —
/// `WORLD_SURFACE`, `OCEAN_FLOOR`, `MOTION_BLOCKING`, `MOTION_BLOCKING_NO_LEAVES`.
pub const HEIGHTMAP_NAMES: [(&str, HeightmapKind); 4] = [
    ("world_surface", HeightmapKind::WorldSurface),
    ("ocean_floor", HeightmapKind::OceanFloor),
    ("motion_blocking", HeightmapKind::MotionBlocking),
    ("motion_blocking_no_leaves", HeightmapKind::MotionBlockingNoLeaves),
];

/// `HeightmapTypeArgument.heightmap()` — `minecraft:heightmap`.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeightmapArg;

impl ArgumentType for HeightmapArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let text = reader.read_unquoted_string();
        match HEIGHTMAP_NAMES.iter().find(|(name, _)| *name == text) {
            Some((_, kind)) => Ok(ParsedValue::dynamic(*kind)),
            None => {
                reader.set_cursor(start);
                Err(ParseError::new(start, ParseErrorKind::InvalidBool(text)))
            }
        }
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        HEIGHTMAP_NAMES.iter().map(|(name, _)| (*name).to_string()).collect()
    }
}

impl McArg for HeightmapArg {
    type Value = HeightmapKind;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Heightmap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<HeightmapKind, ParseError> {
        let mut reader = StringReader::new(text);
        HeightmapArg.parse(&mut reader).map(|value| *value.downcast_ref::<HeightmapKind>().expect("HeightmapKind"))
    }

    #[test]
    fn the_four_surviving_names_parse() {
        assert_eq!(parse("world_surface"), Ok(HeightmapKind::WorldSurface));
        assert_eq!(parse("ocean_floor"), Ok(HeightmapKind::OceanFloor));
        assert_eq!(parse("motion_blocking"), Ok(HeightmapKind::MotionBlocking));
        assert_eq!(parse("motion_blocking_no_leaves"), Ok(HeightmapKind::MotionBlockingNoLeaves));
    }

    #[test]
    fn the_two_worldgen_only_variants_are_not_offered() {
        assert!(parse("world_surface_wg").is_err());
        assert!(parse("ocean_floor_wg").is_err());
    }

    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("bogus");
        assert!(HeightmapArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }

    #[test]
    fn the_wire_identity_names_the_heightmap_parser() {
        assert_eq!(HeightmapArg.wire(), ArgumentParser::Heightmap);
    }
}
