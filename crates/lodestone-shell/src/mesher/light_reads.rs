use std::cell::Cell;

const LIGHT_READ_EDGE: usize = 18;
const LIGHT_READ_CELLS: usize = LIGHT_READ_EDGE * LIGHT_READ_EDGE * LIGHT_READ_EDGE;
const LIGHT_READ_WORDS: usize = LIGHT_READ_CELLS.div_ceil(64);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct LightReadSummary {
    pub(super) distinct_values: u32,
    pub(super) unique_cells: u32,
    pub(super) reads: u64,
    pub(super) out_of_domain_reads: u64,
}

pub(super) struct LightReadProbe {
    values: [Cell<u64>; 4],
    cells: [Cell<u64>; LIGHT_READ_WORDS],
    reads: Cell<u64>,
    out_of_domain_reads: Cell<u64>,
}

impl Default for LightReadProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LightReadProbe {
    pub(super) fn new() -> Self {
        Self {
            values: [const { Cell::new(0) }; 4],
            cells: [const { Cell::new(0) }; LIGHT_READ_WORDS],
            reads: Cell::new(0),
            out_of_domain_reads: Cell::new(0),
        }
    }

    pub(super) fn observe(&self, x: i32, y: i32, z: i32, sky: u8, block: u8) {
        debug_assert!(sky <= 15 && block <= 15);
        self.reads.set(self.reads.get().saturating_add(1));
        let packed = usize::from((sky << 4) | block);
        let value_word = &self.values[packed / 64];
        value_word.set(value_word.get() | (1 << (packed % 64)));

        if !(-1..=16).contains(&x) || !(-1..=16).contains(&y) || !(-1..=16).contains(&z) {
            self.out_of_domain_reads
                .set(self.out_of_domain_reads.get().saturating_add(1));
            return;
        }
        let index = (x + 1) as usize
            + LIGHT_READ_EDGE * ((z + 1) as usize + LIGHT_READ_EDGE * (y + 1) as usize);
        let cell_word = &self.cells[index / 64];
        cell_word.set(cell_word.get() | (1 << (index % 64)));
    }

    pub(super) fn summary(&self) -> LightReadSummary {
        LightReadSummary {
            distinct_values: self.values.iter().map(|word| word.get().count_ones()).sum(),
            unique_cells: self.cells.iter().map(|word| word.get().count_ones()).sum(),
            reads: self.reads.get(),
            out_of_domain_reads: self.out_of_domain_reads.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_read_probe_counts_packed_pairs_and_duplicate_reads() {
        let probe = LightReadProbe::new();
        probe.observe(8, 9, 8, 3, 11);
        probe.observe(8, 9, 8, 3, 11);
        probe.observe(8, 9, 8, 11, 3);
        for x in [-1, 16] {
            for y in [-1, 16] {
                for z in [-1, 16] {
                    probe.observe(x, y, z, 3, 3);
                }
            }
        }
        assert_eq!(probe.summary(), LightReadSummary {
            distinct_values: 3,
            unique_cells: 9,
            reads: 11,
            out_of_domain_reads: 0,
        });
    }

    #[test]
    fn light_read_probe_covers_padded_cube_and_reports_outside_reads() {
        let probe = LightReadProbe::default();
        assert_eq!(probe.summary(), LightReadSummary::default());
        for x in -1..=16 {
            for y in -1..=16 {
                for z in -1..=16 {
                    probe.observe(x, y, z, ((x + 1) % 16) as u8, ((z + 1) % 16) as u8);
                }
            }
        }
        assert_eq!(probe.summary(), LightReadSummary {
            distinct_values: 16 * 16,
            unique_cells: 18 * 18 * 18,
            reads: 18 * 18 * 18,
            out_of_domain_reads: 0,
        });
        for [x, y, z] in [
            [-2, 0, 0], [17, 0, 0],
            [0, -2, 0], [0, 17, 0],
            [0, 0, -2], [0, 0, 17],
        ] {
            probe.observe(x, y, z, 15, 0);
        }
        assert_eq!(probe.summary(), LightReadSummary {
            distinct_values: 16 * 16,
            unique_cells: 18 * 18 * 18,
            reads: 18 * 18 * 18 + 6,
            out_of_domain_reads: 6,
        });
    }
}
