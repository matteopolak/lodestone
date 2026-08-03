//! Vanilla's **fancy** cloud mesh: `clouds.png` voxelized into cells and the
//! visible faces of each extruded, as `CloudRenderer.buildMesh` does.
//!
//! # What it is
//!
//! [`crate::sky::cloud_plane_geometry`] draws vanilla's `FAST` clouds — one
//! camera-centred quad, alpha-tested against `clouds.png`, which reproduces
//! *which cells are filled* without any CPU meshing. This module is the `FANCY`
//! half: the same texture becomes a cell grid, and each filled cell contributes
//! the faces of a box.
//!
//! Deliberately **pure**: cells in, faces out. No GPU, no wgpu types, no camera
//! matrices, no clock. That is what lets the face-culling rules — which are the
//! part that is easy to get subtly wrong and invisible when wrong — be asserted
//! directly.
//!
//! # How it works
//!
//! Straight from `CloudRenderer.java` in the real 26.2 `client.jar`.
//!
//! **Cell packing** (`:60-107`). A texel is an empty cell when its alpha is below
//! 10 (`isCellEmpty`), *not* when it is fully transparent — so faint texels are
//! empty too. A filled cell also records whether each of its four horizontal
//! neighbours is empty, which is what the side-face culling below reads.
//!
//! **Ring enumeration** (`buildMesh`, `:239-256`). Cells are visited in
//! taxicab rings out to `radius`, and kept only when
//! `relative_x² + relative_z² <= radius²` — a *disc*, not a square. Each
//! `(x, ring - |x|)` pair yields two cells, at `+z` and `-z`, except when `z == 0`
//! where it would be the same cell twice.
//!
//! **Face selection** (`buildExtrudedCell`, `:294-326`). This is the interesting
//! part, and it is not "cull faces against neighbours":
//!
//! * `UP` unless the camera is below the layer; `DOWN` unless it is above. Both
//!   when inside it.
//! * A side face is emitted only when that neighbour is empty **and the face
//!   points back toward the camera** — `NORTH` needs `z > 0`, `SOUTH` needs
//!   `z < 0`, `WEST` needs `x > 0`, `EAST` needs `x < 0`, where `x`/`z` are
//!   *camera-relative* cell offsets. So the far side of a cloud is never built.
//!   Cells on the axes (`x == 0` or `z == 0`) legitimately emit fewer faces.
//! * Cells within one cell of the camera additionally emit **all six** faces
//!   flagged [`FLAG_INSIDE_FACE`], which is what keeps the cloud you are standing
//!   in from vanishing as the culling above removes its back faces.
//!
//! [`FLAG_USE_TOP_COLOR`] is `FAST`'s single `DOWN` face flag (`buildFlatCell`),
//! carried here so both modes share one encoding.
//!
//! # How to change it
//!
//! The cell grid is built once per texture, the faces once per camera *cell* — not
//! per frame. Rebuilding is keyed on the camera crossing a cell boundary or the
//! layer being entered/left, so [`CloudRelativePos`] is an input rather than
//! something this module derives from a Y coordinate: the caller already knows the
//! cloud height and its own eye position, and threading a height in here would
//! duplicate that.
//!
//! **One deliberate divergence from vanilla.** `CloudRenderer.java:74` and `:76`
//! wrap the *x* axis by `height` when sampling the east and west neighbours:
//!
//! ```text
//! boolean east = isCellEmpty(texture.getPixel(Math.floorMod(x + 1, height), y));
//! ```
//!
//! `width` is the correct modulus there. It cannot manifest in vanilla because
//! `clouds.png` is 256×256, so the two are equal — [`CloudCells::from_rgba`] uses
//! `width`, and a non-square cloud texture from a resource pack will wrap
//! correctly here and incorrectly in vanilla. Matching a bug that cannot fire on
//! the real asset would be the wrong call, but it is written down rather than
//! silently "fixed".
//!
//! # Configuration
//!
//! None. [`crate::sky::CLOUD_CELL_BLOCKS`] and `CLOUD_HEIGHT` are the geometry
//! constants and live beside the `FAST` path.
//!
//! # Dependencies
//!
//! None beyond `core`. This module is why the fancy path can be developed and
//! asserted with no adapter.

/// A face's outward direction, in vanilla's `Direction.get3DDataValue()` order.
///
/// The numbering is not decorative: `encodeFace` packs it into the low bits of a
/// byte alongside the flags, so it is part of the wire format between the mesh
/// builder and the shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum CloudFaceDir {
    /// `-Y`, vanilla's `Direction.DOWN`.
    Down = 0,
    /// `+Y`, vanilla's `Direction.UP`.
    Up = 1,
    /// `-Z`, vanilla's `Direction.NORTH`.
    North = 2,
    /// `+Z`, vanilla's `Direction.SOUTH`.
    South = 3,
    /// `-X`, vanilla's `Direction.WEST`.
    West = 4,
    /// `+X`, vanilla's `Direction.EAST`.
    East = 5,
}

/// `CloudRenderer.FLAG_INSIDE_FACE` (`:34`). Set on the six faces emitted for
/// cells within one cell of the camera, which are the ones seen from inside.
pub const FLAG_INSIDE_FACE: u8 = 16;

/// `CloudRenderer.FLAG_USE_TOP_COLOR` (`:35`). `FAST`'s single `DOWN` face carries
/// it so the flat mode is shaded as a cloud top rather than an underside.
pub const FLAG_USE_TOP_COLOR: u8 = 32;

/// The alpha below which a texel is an empty cell (`CloudRenderer.isCellEmpty`,
/// `:101-103`).
///
/// **Not zero.** A texel at alpha 9 is empty and one at 10 is filled, so a
/// resource pack's soft cloud edges collapse rather than producing a fringe of
/// one-cell boxes. Testing `alpha > 0` instead would silently fill the whole
/// texture for any pack with anti-aliased clouds.
pub const CELL_EMPTY_ALPHA: u8 = 10;

/// Which side of the cloud layer the camera is on
/// (`CloudRenderer.RelativeCameraPos`).
///
/// An input rather than something derived here — see the module's "How to change
/// it". It selects the horizontal faces: you cannot see the underside from above
/// or the top from below, and vanilla does not build what you cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudRelativePos {
    /// Eye above the layer: no `DOWN` faces.
    AboveClouds,
    /// Eye within the layer's own thickness: both `UP` and `DOWN`.
    InsideClouds,
    /// Eye below the layer: no `UP` faces.
    BelowClouds,
}

/// One face to draw, in **camera-relative cell** coordinates.
///
/// `cell_x`/`cell_z` are offsets from the camera's own cell, so `(0, 0)` is the
/// cell the camera is in. The caller scales them by
/// [`crate::sky::CLOUD_CELL_BLOCKS`] and offsets by the layer height; keeping this
/// in cells means the mesh is valid until the camera crosses a cell boundary
/// rather than for exactly one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudFace {
    /// Cell offset along `+X` from the camera's cell.
    pub cell_x: i32,
    /// Cell offset along `+Z` from the camera's cell.
    pub cell_z: i32,
    /// Which way the face points.
    pub dir: CloudFaceDir,
    /// [`FLAG_INSIDE_FACE`] / [`FLAG_USE_TOP_COLOR`], or 0.
    pub flags: u8,
}

/// Per-cell occupancy plus which horizontal neighbours are empty.
///
/// One byte per cell: bit 4 is "filled", bits 3..0 are north/east/south/west
/// "neighbour is empty", matching `packCellData`'s bit order (`:105-107`) minus
/// the packed colour, which this renderer does not use — the cloud colour is a
/// single uniform (`crate::sky::CLOUD_COLOR_RGB`), not per-texel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudCells {
    width: u32,
    height: u32,
    cells: Vec<u8>,
}

const FILLED: u8 = 1 << 4;
const NORTH_EMPTY: u8 = 1 << 3;
const EAST_EMPTY: u8 = 1 << 2;
const SOUTH_EMPTY: u8 = 1 << 1;
const WEST_EMPTY: u8 = 1 << 0;

impl CloudCells {
    /// Voxelizes an RGBA8 `clouds.png` into cells.
    ///
    /// `rgba` is row-major, four bytes per texel. A short buffer is treated as
    /// all-empty rather than panicking: a cloud layer that fails to draw is a
    /// better outcome than a crash on a malformed resource pack.
    ///
    /// Neighbour wrapping is `floorMod`, so the texture tiles — see the module's
    /// note on vanilla's `height`-vs-`width` slip.
    #[must_use]
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return Self {
                width: 0,
                height: 0,
                cells: Vec::new(),
            };
        }
        let alpha_at = |x: usize, y: usize| rgba[(x + y * w) * 4 + 3];
        let empty_at = |x: usize, y: usize| alpha_at(x, y) < CELL_EMPTY_ALPHA;

        let mut cells = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                if empty_at(x, y) {
                    continue;
                }
                let mut packed = FILLED;
                // `floorMod` on `usize` is just the wrap, and `+ w - 1` avoids an
                // underflow at x == 0 that a plain `x - 1` would hit.
                if empty_at(x, (y + h - 1) % h) {
                    packed |= NORTH_EMPTY;
                }
                if empty_at((x + 1) % w, y) {
                    packed |= EAST_EMPTY;
                }
                if empty_at(x, (y + 1) % h) {
                    packed |= SOUTH_EMPTY;
                }
                if empty_at((x + w - 1) % w, y) {
                    packed |= WEST_EMPTY;
                }
                cells[x + y * w] = packed;
            }
        }
        Self {
            width,
            height,
            cells,
        }
    }

    /// Texture dimensions in cells.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Whether any cell is filled. `false` for an all-transparent or malformed
    /// texture, which is the caller's cue to skip the pass entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|c| *c & FILLED == 0)
    }

    /// The packed byte for an absolute cell, wrapping both axes.
    fn packed(&self, cell_x: i32, cell_z: i32) -> u8 {
        if self.width == 0 || self.height == 0 {
            return 0;
        }
        let x = cell_x.rem_euclid(self.width as i32) as usize;
        let y = cell_z.rem_euclid(self.height as i32) as usize;
        self.cells[x + y * self.width as usize]
    }
}

/// Builds the visible faces of every filled cell within `radius_cells` of the
/// camera's cell — vanilla's `buildMesh` with `extrude = true`.
///
/// `center_cell_x`/`center_cell_z` are the camera's **absolute** cell, which is
/// what indexes the texture; the returned faces are camera-*relative*.
///
/// Order matches vanilla's ring walk, which matters only in that it is
/// front-to-back-ish and therefore friendly to the translucent blend; nothing
/// depends on it for correctness.
#[must_use]
pub fn extruded_faces(
    cells: &CloudCells,
    center_cell_x: i32,
    center_cell_z: i32,
    radius_cells: i32,
    relative_pos: CloudRelativePos,
) -> Vec<CloudFace> {
    let mut faces = Vec::new();
    if radius_cells < 0 || cells.is_empty() {
        return faces;
    }
    for_each_cell_in_disc(radius_cells, |rx, rz| {
        let packed = cells.packed(center_cell_x + rx, center_cell_z + rz);
        if packed & FILLED == 0 {
            return;
        }
        push_extruded_cell(&mut faces, rx, rz, packed, relative_pos);
    });
    faces
}

/// Vanilla's `buildMesh` with `extrude = false`: one `DOWN` face per filled cell,
/// flagged [`FLAG_USE_TOP_COLOR`].
///
/// Present so `FAST` and `FANCY` come out of one enumeration rather than two
/// different code paths that could disagree about *which* cells are filled. Note
/// the shipped `FAST` path today is a single alpha-tested quad
/// ([`crate::sky::cloud_plane_geometry`]) and does not use this — this is the
/// faithful mesh version, and having both makes the comparison testable.
#[must_use]
pub fn flat_faces(
    cells: &CloudCells,
    center_cell_x: i32,
    center_cell_z: i32,
    radius_cells: i32,
) -> Vec<CloudFace> {
    let mut faces = Vec::new();
    if radius_cells < 0 || cells.is_empty() {
        return faces;
    }
    for_each_cell_in_disc(radius_cells, |rx, rz| {
        if cells.packed(center_cell_x + rx, center_cell_z + rz) & FILLED == 0 {
            return;
        }
        faces.push(CloudFace {
            cell_x: rx,
            cell_z: rz,
            dir: CloudFaceDir::Down,
            flags: FLAG_USE_TOP_COLOR,
        });
    });
    faces
}

/// Vanilla's ring walk (`buildMesh`, `:239-256`): taxicab rings clipped to a disc.
///
/// Factored out so both mesh modes enumerate identically. The `rz != 0` guard is
/// vanilla's and is not an optimisation — without it every on-axis cell would be
/// visited twice and emit doubled faces.
fn for_each_cell_in_disc(radius_cells: i32, mut visit: impl FnMut(i32, i32)) {
    for ring in 0..=(2 * radius_cells) {
        for rx in -ring..=ring {
            let rz = ring - rx.abs();
            if rz < 0 || rz > radius_cells || rx * rx + rz * rz > radius_cells * radius_cells {
                continue;
            }
            if rz != 0 {
                visit(rx, -rz);
            }
            visit(rx, rz);
        }
    }
}

/// `buildExtrudedCell` (`:294-326`).
fn push_extruded_cell(
    faces: &mut Vec<CloudFace>,
    x: i32,
    z: i32,
    packed: u8,
    relative_pos: CloudRelativePos,
) {
    let mut push = |dir, flags| faces.push(CloudFace {
        cell_x: x,
        cell_z: z,
        dir,
        flags,
    });

    if relative_pos != CloudRelativePos::BelowClouds {
        push(CloudFaceDir::Up, 0);
    }
    if relative_pos != CloudRelativePos::AboveClouds {
        push(CloudFaceDir::Down, 0);
    }
    // A side face only when the neighbour is empty *and* the face turns back
    // toward the camera. The second half is why an on-axis cell emits fewer
    // faces, and why the far wall of a cloud bank is never built at all.
    if packed & NORTH_EMPTY != 0 && z > 0 {
        push(CloudFaceDir::North, 0);
    }
    if packed & SOUTH_EMPTY != 0 && z < 0 {
        push(CloudFaceDir::South, 0);
    }
    if packed & WEST_EMPTY != 0 && x > 0 {
        push(CloudFaceDir::West, 0);
    }
    if packed & EAST_EMPTY != 0 && x < 0 {
        push(CloudFaceDir::East, 0);
    }
    // The cell the camera is in, and its eight neighbours, get a full box with
    // every face flagged — otherwise the culling above would strip exactly the
    // faces you are looking at from inside.
    if x.abs() <= 1 && z.abs() <= 1 {
        for dir in [
            CloudFaceDir::Down,
            CloudFaceDir::Up,
            CloudFaceDir::North,
            CloudFaceDir::South,
            CloudFaceDir::West,
            CloudFaceDir::East,
        ] {
            push(dir, FLAG_INSIDE_FACE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A texture with one filled texel at `(cx, cz)`, alpha `a`, everything else
    /// transparent.
    fn one_texel(w: u32, h: u32, cx: u32, cz: u32, a: u8) -> Vec<u8> {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let i = ((cx + cz * w) * 4) as usize;
        rgba[i] = 255;
        rgba[i + 1] = 255;
        rgba[i + 2] = 255;
        rgba[i + 3] = a;
        rgba
    }

    /// `isCellEmpty` is `alpha < 10`, and the boundary is where a wrong
    /// `alpha > 0` or `alpha == 255` test shows up. Both neighbours of the
    /// threshold are asserted, because a one-sided check passes on `<= 10` too.
    #[test]
    fn the_empty_threshold_is_alpha_below_ten_exactly() {
        for (alpha, expect_filled) in [(0, false), (9, false), (10, true), (11, true), (255, true)] {
            let cells = CloudCells::from_rgba(4, 4, &one_texel(4, 4, 1, 1, alpha));
            assert_eq!(
                !cells.is_empty(),
                expect_filled,
                "alpha {alpha} should be {}",
                if expect_filled { "filled" } else { "empty" }
            );
        }
    }

    /// An isolated cell has all four neighbours empty, so it may emit all four
    /// side faces — subject to the camera-facing rule, which this places it to
    /// satisfy for north and west.
    #[test]
    fn an_isolated_cell_reports_every_neighbour_empty() {
        let cells = CloudCells::from_rgba(8, 8, &one_texel(8, 8, 4, 4, 255));
        let faces = extruded_faces(&cells, 0, 0, 8, CloudRelativePos::AboveClouds);
        let sides: Vec<_> = faces
            .iter()
            .filter(|f| f.cell_x == 4 && f.cell_z == 4 && f.flags == 0)
            .map(|f| f.dir)
            .collect();
        // Above the layer: UP but no DOWN. At (+4, +4): north and west face back
        // toward the camera; south and east point away and are culled.
        assert!(sides.contains(&CloudFaceDir::Up), "{sides:?}");
        assert!(!sides.contains(&CloudFaceDir::Down), "above ⇒ no DOWN: {sides:?}");
        assert!(sides.contains(&CloudFaceDir::North), "{sides:?}");
        assert!(sides.contains(&CloudFaceDir::West), "{sides:?}");
        assert!(!sides.contains(&CloudFaceDir::South), "+z ⇒ no SOUTH: {sides:?}");
        assert!(!sides.contains(&CloudFaceDir::East), "+x ⇒ no EAST: {sides:?}");
    }

    /// The mirror of the above, and the reason it is a separate test: a build that
    /// ignored the sign conditions entirely would pass the previous test's
    /// positive assertions. Mirroring the cell to `(-x, -z)` must mirror the faces.
    #[test]
    fn the_camera_facing_rule_mirrors_with_the_cell() {
        let cells = CloudCells::from_rgba(8, 8, &one_texel(8, 8, 4, 4, 255));
        // Centre the camera so the filled texel lands at relative (-4, -4).
        let faces = extruded_faces(&cells, 8, 8, 8, CloudRelativePos::AboveClouds);
        let sides: Vec<_> = faces
            .iter()
            .filter(|f| f.cell_x == -4 && f.cell_z == -4 && f.flags == 0)
            .map(|f| f.dir)
            .collect();
        assert!(sides.contains(&CloudFaceDir::South), "{sides:?}");
        assert!(sides.contains(&CloudFaceDir::East), "{sides:?}");
        assert!(!sides.contains(&CloudFaceDir::North), "{sides:?}");
        assert!(!sides.contains(&CloudFaceDir::West), "{sides:?}");
    }

    /// A filled neighbour must suppress the shared face. Two adjacent cells along
    /// x: the west one's east neighbour is filled and vice versa.
    #[test]
    fn a_filled_neighbour_suppresses_the_shared_face() {
        let (w, h) = (8u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for x in [4u32, 5] {
            let i = ((x + 4 * w) * 4) as usize;
            rgba[i + 3] = 255;
        }
        let cells = CloudCells::from_rgba(w, h, &rgba);
        // Camera far to +x of both, so WEST faces turn back toward it.
        let faces = extruded_faces(&cells, 0, 0, 8, CloudRelativePos::AboveClouds);
        let west_of_5 = faces
            .iter()
            .any(|f| f.cell_x == 5 && f.cell_z == 4 && f.dir == CloudFaceDir::West && f.flags == 0);
        let west_of_4 = faces
            .iter()
            .any(|f| f.cell_x == 4 && f.cell_z == 4 && f.dir == CloudFaceDir::West && f.flags == 0);
        assert!(
            !west_of_5,
            "cell 5's west neighbour (cell 4) is filled, so that face must be culled"
        );
        assert!(
            west_of_4,
            "control: cell 4's west neighbour is empty, so its west face must exist \
             — without this the assertion above passes on a build emitting no side faces"
        );
    }

    /// `AboveClouds` drops `DOWN`, `BelowClouds` drops `UP`, `InsideClouds` keeps
    /// both. Asserted as a set over all three, because any two of them alone are
    /// satisfied by a build that always omits the same one.
    #[test]
    fn the_horizontal_faces_follow_which_side_the_camera_is_on() {
        let cells = CloudCells::from_rgba(8, 8, &one_texel(8, 8, 4, 4, 255));
        for (pos, up, down) in [
            (CloudRelativePos::AboveClouds, true, false),
            (CloudRelativePos::BelowClouds, false, true),
            (CloudRelativePos::InsideClouds, true, true),
        ] {
            let faces = extruded_faces(&cells, 0, 0, 8, pos);
            let unflagged: Vec<_> = faces
                .iter()
                .filter(|f| f.cell_x == 4 && f.cell_z == 4 && f.flags == 0)
                .map(|f| f.dir)
                .collect();
            assert_eq!(
                unflagged.contains(&CloudFaceDir::Up),
                up,
                "{pos:?} UP: {unflagged:?}"
            );
            assert_eq!(
                unflagged.contains(&CloudFaceDir::Down),
                down,
                "{pos:?} DOWN: {unflagged:?}"
            );
        }
    }

    /// Interior faces are emitted for the 3×3 around the camera and nowhere else,
    /// and there are exactly six of them per such cell.
    #[test]
    fn interior_faces_cover_the_nine_cells_around_the_camera_and_no_others() {
        // Fill everything, so cell occupancy cannot be what limits the result.
        let (w, h) = (16u32, 16u32);
        let rgba: Vec<u8> = [0u8, 0, 0, 255].repeat((w * h) as usize);
        let cells = CloudCells::from_rgba(w, h, &rgba);
        let faces = extruded_faces(&cells, 0, 0, 6, CloudRelativePos::InsideClouds);

        for (x, z) in [(0, 0), (1, 0), (-1, 1), (1, -1), (-1, -1)] {
            let n = faces
                .iter()
                .filter(|f| f.cell_x == x && f.cell_z == z && f.flags == FLAG_INSIDE_FACE)
                .count();
            assert_eq!(n, 6, "({x},{z}) must carry all six interior faces");
        }
        for (x, z) in [(2, 0), (0, 2), (2, 2), (-2, 1), (3, -3)] {
            let n = faces
                .iter()
                .filter(|f| f.cell_x == x && f.cell_z == z && f.flags == FLAG_INSIDE_FACE)
                .count();
            assert_eq!(n, 0, "({x},{z}) is outside the 3x3 and must carry none");
        }
    }

    /// The enumeration is a **disc**, not a square, and visits each cell exactly
    /// once. A doubled visit would double every face and is invisible in a render.
    #[test]
    fn the_ring_walk_covers_a_disc_exactly_once() {
        let radius = 5;
        let mut seen = Vec::new();
        for_each_cell_in_disc(radius, |x, z| seen.push((x, z)));

        let mut sorted = seen.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a cell was visited twice");

        // Every cell inside the disc is present...
        for x in -radius..=radius {
            for z in -radius..=radius {
                let inside = x * x + z * z <= radius * radius;
                assert_eq!(
                    seen.contains(&(x, z)),
                    inside,
                    "({x},{z}) inside={inside} but present={}",
                    seen.contains(&(x, z))
                );
            }
        }
        // ...and the corners of the bounding square are not, which is what
        // distinguishes a disc from a square.
        assert!(!seen.contains(&(radius, radius)));
    }

    /// `flat_faces` is one `DOWN` face per filled cell with the top-colour flag —
    /// vanilla's `buildFlatCell` — and it must agree with `extruded_faces` about
    /// *which* cells are filled, since they share the enumeration.
    #[test]
    fn flat_mode_is_one_top_coloured_down_face_per_filled_cell() {
        // 16×16 with radius 4, so the texture's wrap period is larger than the
        // disc and the single filled cell appears exactly once. Using a radius at
        // or beyond the texture size instead makes it appear four times — which is
        // correct (see `the_pattern_tiles_when_the_radius_exceeds_the_texture`)
        // and would make "one face per filled cell" the wrong assertion.
        let cells = CloudCells::from_rgba(16, 16, &one_texel(16, 16, 2, 2, 255));
        let flat = flat_faces(&cells, 0, 0, 4);
        assert_eq!(flat.len(), 1, "{flat:?}");
        assert_eq!(flat[0].dir, CloudFaceDir::Down);
        assert_eq!(flat[0].flags, FLAG_USE_TOP_COLOR);
        assert_eq!((flat[0].cell_x, flat[0].cell_z), (2, 2));

        let extruded = extruded_faces(&cells, 0, 0, 4, CloudRelativePos::AboveClouds);
        let flat_cells: Vec<_> = flat.iter().map(|f| (f.cell_x, f.cell_z)).collect();
        let mut ex_cells: Vec<_> = extruded.iter().map(|f| (f.cell_x, f.cell_z)).collect();
        ex_cells.sort_unstable();
        ex_cells.dedup();
        assert_eq!(flat_cells, ex_cells, "the two modes must fill the same cells");
    }

    /// **The pattern tiles.** A view radius at or beyond the texture's size sees
    /// the same cell more than once, because the lookup wraps — one filled texel
    /// in an 8×8 texture appears at four relative offsets within radius 8, since
    /// `rem_euclid(8)` maps −4 and +4 to the same texel.
    ///
    /// This is vanilla's behaviour too (`tryBuildCell`'s `Math.floorMod`) and is
    /// why `clouds.png` is 256 cells across: at 12 blocks per cell that is 3072
    /// blocks of period, far beyond any render distance, so a player never sees
    /// the repeat. It is pinned here because it caught this file's own first
    /// test — which asserted one face per filled cell at a radius that wrapped
    /// three times — and because a future "why are there duplicate faces?" reads
    /// as a bug without it.
    #[test]
    fn the_pattern_tiles_when_the_radius_exceeds_the_texture() {
        let cells = CloudCells::from_rgba(8, 8, &one_texel(8, 8, 4, 4, 255));
        let at = |r| {
            let mut c: Vec<_> = flat_faces(&cells, 0, 0, r)
                .iter()
                .map(|f| (f.cell_x, f.cell_z))
                .collect();
            c.sort_unstable();
            c
        };
        // Radius 3 < 8: the texel at (4,4) is outside the disc entirely.
        assert_eq!(at(3), Vec::new(), "radius 3 cannot reach cell (4,4)");
        // Radius 8: all four wrapped images, and no more.
        assert_eq!(at(8), vec![(-4, -4), (-4, 4), (4, -4), (4, 4)]);
    }

    /// Neighbour lookups wrap, so a cell on the texture's edge sees across the
    /// seam. Two filled texels at x = 0 and x = w-1 are neighbours.
    #[test]
    fn neighbour_lookups_wrap_across_the_texture_seam() {
        let (w, h) = (8u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for x in [0u32, w - 1] {
            rgba[((x + 4 * w) * 4) as usize + 3] = 255;
        }
        let cells = CloudCells::from_rgba(w, h, &rgba);
        // Cell 0's west neighbour is cell w-1, which is filled ⇒ not empty.
        assert_eq!(cells.packed(0, 4) & WEST_EMPTY, 0, "the seam must wrap");
        // Control: its north neighbour genuinely is empty.
        assert_ne!(cells.packed(0, 4) & NORTH_EMPTY, 0);
    }

    /// A malformed or empty texture yields no faces rather than panicking, and
    /// `is_empty` is the caller's signal to skip the pass (vanilla skips it when
    /// the status is OFF *or* the cloud alpha is 0).
    #[test]
    fn a_degenerate_texture_is_empty_and_yields_no_faces() {
        for cells in [
            CloudCells::from_rgba(0, 0, &[]),
            CloudCells::from_rgba(4, 4, &[0, 0, 0]),
            CloudCells::from_rgba(4, 4, &vec![0u8; 4 * 4 * 4]),
        ] {
            assert!(cells.is_empty());
            assert!(extruded_faces(&cells, 0, 0, 8, CloudRelativePos::InsideClouds).is_empty());
            assert!(flat_faces(&cells, 0, 0, 8).is_empty());
        }
    }
}
