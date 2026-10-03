use crate::LightData;

/// Affected section offsets and changed-cell bounds, packed into eight bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LightBoundaryMask(u64);

impl LightBoundaryMask {
    /// Conservatively includes the source section and all 26 neighbours.
    pub const ALL: Self = Self(
        (1 << 27) - 1 | (0xF0 << 27) | (0xF0 << 35) | (0xF0 << 43),
    );

    /// Whether a target section at `(dx, dy, dz)` can read the changed light.
    #[must_use]
    pub fn contains(self, dx: i32, dy: i32, dz: i32) -> bool {
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        self.0 & (1 << ((dx + 1) * 9 + (dy + 1) * 3 + dz + 1)) != 0
    }

    /// Whether no stored nibble changed.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Adds another layer or changed cell's affected section offsets.
    pub fn union(&mut self, other: Self) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other;
            return;
        }
        let mut merged = (self.0 | other.0) & ((1 << 27) - 1);
        for shift in [27, 35, 43] {
            let a = (self.0 >> shift) & 255;
            let b = (other.0 >> shift) & 255;
            merged |= ((a & 15).min(b & 15) | ((a >> 4).max(b >> 4) << 4)) << shift;
        }
        self.0 = merged;
    }

    /// The affected offsets for one changed local nibble coordinate.
    #[must_use]
    pub fn for_cell(x: usize, y: usize, z: usize) -> Self {
        assert!(x < 16 && y < 16 && z < 16);
        let mut bits = 1u64 << 13;
        for (coordinate, shift) in [(x, 9), (y, 3), (z, 1)] {
            if coordinate < 2 {
                bits |= bits >> shift;
            } else if coordinate >= 14 {
                bits |= bits << shift;
            }
        }
        for (coordinate, shift) in [(x, 27), (y, 35), (z, 43)] {
            bits |= (coordinate as u64 * 17) << shift;
        }
        Self(bits)
    }

    /// Inclusive local block bounds that can sample changed light, with radius two.
    #[must_use]
    pub fn affected_blocks(self, dx: i32, dy: i32, dz: i32) -> Option<([usize; 3], [usize; 3])> {
        if !self.contains(dx, dy, dz) {
            return None;
        }
        let mut lo = [0; 3];
        let mut hi = [0; 3];
        for (axis, (offset, shift)) in [(dx, 27), (dy, 35), (dz, 43)].into_iter().enumerate() {
            let bounds = (self.0 >> shift) & 255;
            lo[axis] = ((bounds & 15) as i32 - offset * 16 - 2).max(0) as usize;
            hi[axis] = ((bounds >> 4) as i32 - offset * 16 + 2).min(15) as usize;
        }
        Some((lo, hi))
    }

    pub(crate) fn between(before: &LightData, after: &LightData) -> Self {
        if before == after {
            return Self::default();
        }
        if matches!(before, LightData::Missing) || matches!(after, LightData::Missing)
            || matches!((before, after), (LightData::Uniform(_), LightData::Uniform(_)))
        {
            return Self::ALL;
        }
        let mut mask = Self::default();
        for index in 0..4096 {
            if before.get(index) != after.get(index) {
                mask.union(Self::for_cell(index & 15, index >> 8, (index >> 4) & 15));
                if mask == Self::ALL {
                    break;
                }
            }
        }
        mask
    }
}

/// Changed light in packet coordinates, including the two vertical sentinel layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightSectionChange {
    /// Light index `i` covers block section `i - 1`.
    pub section_index: usize,
    /// Union of the sky and block layers' affected target offsets.
    pub affected: LightBoundaryMask,
}

impl LightSectionChange {
    /// A compatibility change without stored-nibble readback.
    #[must_use]
    pub const fn whole_section(section_index: usize) -> Self {
        Self { section_index, affected: LightBoundaryMask::ALL }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NibbleArray;

    #[test]
    fn changed_cell_offsets_match_independent_padded_boxes() {
        let cells = [
            ([7, 9, 5], 1),
            ([0, 9, 5], 2), ([15, 9, 5], 2),
            ([1, 9, 5], 2), ([14, 9, 5], 2),
            ([7, 0, 5], 2), ([7, 15, 5], 2),
            ([7, 9, 0], 2), ([7, 9, 15], 2),
            ([0, 15, 5], 4), ([15, 0, 15], 8),
        ];
        for ([x, y, z], expected_count) in cells {
            let mask = LightBoundaryMask::for_cell(x, y, z);
            let mut count = 0;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let inside = [x as i32 - dx * 16, y as i32 - dy * 16, z as i32 - dz * 16]
                            .into_iter().all(|coordinate| (-2..=17).contains(&coordinate));
                        assert_eq!(mask.contains(dx, dy, dz), inside, "cell={x},{y},{z} offset={dx},{dy},{dz}");
                        count += usize::from(inside);
                    }
                }
            }
            assert_eq!(count, expected_count);
        }
        assert_ne!(LightBoundaryMask::ALL, LightBoundaryMask::for_cell(7, 9, 5));
    }

    #[test]
    fn changed_bounds_union_without_inventing_offsets() {
        assert_eq!(std::mem::size_of::<LightBoundaryMask>(), 8);
        let mut mask = LightBoundaryMask::for_cell(7, 9, 5);
        assert_eq!(mask.affected_blocks(0, 0, 0), Some(([5, 7, 3], [9, 11, 7])));
        assert_eq!(mask.affected_blocks(1, 0, 0), None);
        mask.union(LightBoundaryMask::for_cell(14, 7, 8));
        assert_eq!(mask.affected_blocks(1, 0, 0), Some(([0, 5, 3], [0, 11, 10])));
        mask.union(LightBoundaryMask::for_cell(7, 1, 5));
        assert!(mask.contains(0, -1, 0));
        assert!(!mask.contains(1, -1, 0));
        mask.union(LightBoundaryMask::default());
        mask.union(LightBoundaryMask::ALL);
        assert_eq!(mask, LightBoundaryMask::ALL);
    }

    #[test]
    fn nibble_diff_respects_both_halves_and_missing_transitions() {
        let before = LightData::Uniform(3);
        let mut values = NibbleArray::filled(3);
        values.set(9 * 256 + 5 * 16 + 7, 11);
        let after = LightData::Values(values);
        assert_eq!(LightBoundaryMask::between(&before, &after), LightBoundaryMask::for_cell(7, 9, 5));
        assert!(LightBoundaryMask::between(&after, &after).is_empty());
        assert!(LightBoundaryMask::between(&before, &LightData::Values(NibbleArray::filled(3))).is_empty());
        assert_eq!(LightBoundaryMask::between(&LightData::Missing, &before), LightBoundaryMask::ALL);
        assert_eq!(LightBoundaryMask::between(&before, &LightData::Uniform(0)), LightBoundaryMask::ALL);
    }
}
