//! Block positions and the decoration random source.

use lodestone_worldgen_core::rng::{WorldgenRandom, XoroshiroRandomSource};

use crate::blocks::Dir;

/// The random source features draw from (a legacy-shaped wrapper over xoroshiro bits).
pub type Rng = WorldgenRandom<XoroshiroRandomSource>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Pos {
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub const fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self { x: self.x + dx, y: self.y + dy, z: self.z + dz }
    }

    #[must_use]
    pub const fn at_y(self, y: i32) -> Self {
        Self { x: self.x, y, z: self.z }
    }

    #[must_use]
    pub fn relative(self, d: Dir) -> Self {
        let (dx, dy, dz) = d.step();
        self.offset(dx, dy, dz)
    }

    #[must_use]
    pub fn relative_n(self, d: Dir, n: i32) -> Self {
        let (dx, dy, dz) = d.step();
        self.offset(dx * n, dy * n, dz * n)
    }

    #[must_use]
    pub const fn above(self) -> Self {
        self.offset(0, 1, 0)
    }

    #[must_use]
    pub const fn below(self) -> Self {
        self.offset(0, -1, 0)
    }
}
