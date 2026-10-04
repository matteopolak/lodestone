//! A rectangular lattice of block positions, laid out Y-fastest, then X, then Z.

/// The sampled region: `size` lattice points starting at `min`, `step` blocks apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Volume {
    pub size: [i32; 3],
    pub min: [i32; 3],
    pub step: [i32; 3],
}

impl Volume {
    pub fn new(size: [i32; 3], min: [i32; 3], step: [i32; 3]) -> Self {
        assert!(size.iter().all(|&s| s > 0), "volume size must be positive: {size:?}");
        assert!(step.iter().all(|&s| s > 0), "volume step must be positive: {step:?}");
        Self { size, min, step }
    }

    /// A volume of consecutive blocks.
    pub fn blocks(size: [i32; 3], min: [i32; 3]) -> Self {
        Self::new(size, min, [1, 1, 1])
    }

    #[inline]
    pub fn block_x(&self, x: i32) -> i32 {
        self.min[0].wrapping_add(x.wrapping_mul(self.step[0]))
    }

    #[inline]
    pub fn block_y(&self, y: i32) -> i32 {
        self.min[1].wrapping_add(y.wrapping_mul(self.step[1]))
    }

    #[inline]
    pub fn block_z(&self, z: i32) -> i32 {
        self.min[2].wrapping_add(z.wrapping_mul(self.step[2]))
    }

    pub fn max_block(&self, axis: usize) -> i32 {
        self.min[axis] + self.size[axis] * self.step[axis] - 1
    }

    #[inline]
    pub fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y + (x + z * self.size[0]) * self.size[1]) as usize
    }

    pub fn len(&self) -> usize {
        (self.size[0] * self.size[1] * self.size[2]) as usize
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Index of the lattice point holding this block, if any.
    pub fn index_of_block(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let rel = [x.wrapping_sub(self.min[0]), y.wrapping_sub(self.min[1]), z.wrapping_sub(self.min[2])];
        if self.step == [1, 1, 1] {
            if (0..3).all(|a| rel[a] >= 0 && rel[a] < self.size[a]) {
                return Some(self.index(rel[0], rel[1], rel[2]));
            }
            return None;
        }
        let inside = (0..3).all(|a| {
            rel[a] >= 0 && rel[a] < self.size[a] * self.step[a] && rel[a].rem_euclid(self.step[a]) == 0
        });
        inside.then(|| self.index(rel[0] / self.step[0], rel[1] / self.step[1], rel[2] / self.step[2]))
    }
}
