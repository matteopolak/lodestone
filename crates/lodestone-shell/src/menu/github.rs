//! The title screen's GitHub link: the repository URL and the pixel-art mark
//! drawn on the corner button.

use lodestone_assets::Image;

/// The project repository the title screen's GitHub button opens. A constant of
/// this client, not server-supplied, so opening it needs no confirmation screen.
pub const REPOSITORY_URL: &str = "https://github.com/matteopolak/lodestone";

/// The menu atlas id the mark is registered under (see
/// [`crate::resources::load_menu_gui_atlas`]).
pub const ICON_SPRITE: &str = "lodestone/github";

/// Side of the mark, in pixels. The button draws icons at 15×15, so a 15×15
/// source maps one texel to one logical pixel.
const SIDE: usize = 15;

/// The mark, drawn by hand: a filled disc with the cat cut out of it (ears,
/// head, the tail curling left, the body running out through the bottom
/// edge). `#` is opaque, `.` is transparent.
const ART: [&str; SIDE] = [
    ".....#####.....",
    "...#########...",
    "..###########..",
    ".###.#####.###.",
    ".###..###..###.",
    "####.......####",
    "####.......####",
    "####.......####",
    "####.......####",
    "#####.....#####",
    ".##.##...#####.",
    ".###.....#####.",
    "..####...####..",
    "...###...###...",
    ".....#...#.....",
];

/// The mark as an RGBA image: opaque white where [`ART`] has `#`, fully
/// transparent elsewhere.
#[must_use]
pub fn icon_image() -> Image {
    let mut rgba = Vec::with_capacity(SIDE * SIDE * 4);
    for row in ART {
        for cell in row.bytes() {
            rgba.extend_from_slice(if cell == b'#' { &[255, 255, 255, 255] } else { &[0, 0, 0, 0] });
        }
    }
    Image { width: SIDE as u32, height: SIDE as u32, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_art_row_is_the_declared_width() {
        for (i, row) in ART.iter().enumerate() {
            assert_eq!(row.len(), SIDE, "row {i} of the mark is {} wide", row.len());
        }
    }

    #[test]
    fn the_mark_is_a_disc_with_the_cat_cut_out() {
        let image = icon_image();
        assert_eq!((image.width, image.height), (15, 15));
        let alpha = |x: usize, y: usize| image.rgba[(y * SIDE + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "the corner outside the disc is clear");
        assert_eq!(alpha(0, 7), 255, "the disc's left rim is ink");
        assert_eq!(alpha(7, 7), 0, "the cat's head is cut out");
        assert_eq!(alpha(7, 14), 0, "the body leaves through the bottom edge");
    }

    #[test]
    fn the_url_is_https_and_parses_as_the_repository() {
        assert_eq!(REPOSITORY_URL, "https://github.com/matteopolak/lodestone");
    }
}
