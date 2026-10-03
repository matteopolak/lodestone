use std::cell::Cell;

const LIGHT_READ_EDGE: usize = 20;
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
    measured: bool,
    uniform: Cell<u16>,
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
        Self::with_measurement(true)
    }

    pub(super) fn with_measurement(measured: bool) -> Self {
        Self {
            measured,
            uniform: Cell::new(256),
            values: [const { Cell::new(0) }; 4],
            cells: [const { Cell::new(0) }; LIGHT_READ_WORDS],
            reads: Cell::new(0),
            out_of_domain_reads: Cell::new(0),
        }
    }

    pub(super) fn observe(&self, x: i32, y: i32, z: i32, sky: u8, block: u8) {
        debug_assert!(sky <= 15 && block <= 15);
        let packed = usize::from((sky << 4) | block);
        self.uniform.set(match self.uniform.get() {
            256 => packed as u16,
            prior if prior == packed as u16 => prior,
            _ => 257,
        });
        if self.measured {
            self.reads.set(self.reads.get().saturating_add(1));
            let value_word = &self.values[packed / 64];
            value_word.set(value_word.get() | (1 << (packed % 64)));
        }

        if !(-2..=17).contains(&x) || !(-2..=17).contains(&y) || !(-2..=17).contains(&z) {
            self.out_of_domain_reads
                .set(self.out_of_domain_reads.get().saturating_add(1));
            return;
        }
        let index = (x + 2) as usize
            + LIGHT_READ_EDGE * ((z + 2) as usize + LIGHT_READ_EDGE * (y + 2) as usize);
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

    pub(super) fn finish(&self, mut read: impl FnMut([i32; 3]) -> u8) -> Option<LightInputs> {
        if self.out_of_domain_reads.get() != 0 { return None }
        let cells = self.cells.each_ref().map(|word| word.get());
        let count: usize = cells.iter().map(|word| word.count_ones() as usize).sum();
        if count == 0 { return Some(LightInputs::Empty) }
        if count >= 250 && self.uniform.get() < 256 {
            return Some(LightInputs::Uniform(Box::new(cells), self.uniform.get() as u8));
        }
        if count > 512 { return None }
        let mut entries = Vec::with_capacity(count);
        for (word_index, mut bits) in cells.into_iter().enumerate() {
            while bits != 0 {
                let index = word_index * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let levels = if self.uniform.get() < 256 {
                    self.uniform.get() as u8
                } else {
                    read(coordinate(index))
                };
                entries.push(((index as u32) << 8) | u32::from(levels));
            }
        }
        Some(LightInputs::Sparse(entries.into_boxed_slice()))
    }
}

fn coordinate(index: usize) -> [i32; 3] {
    [(index % 20) as i32 - 2, (index / 400) as i32 - 2, ((index / 20) % 20) as i32 - 2]
}

#[derive(Debug)]
pub(super) enum LightInputs {
    Empty,
    Uniform(Box<[u64; LIGHT_READ_WORDS]>, u8),
    Sparse(Box<[u32]>),
}

impl LightInputs {
    pub(super) fn retained_bytes(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Uniform(_, _) => LIGHT_READ_WORDS * 8,
            Self::Sparse(entries) => entries.len() * 4,
        }
    }

    pub(super) fn unchanged(
        &self,
        lo: [i32; 3],
        hi: [i32; 3],
        mut read: impl FnMut([i32; 3]) -> u8,
    ) -> (bool, usize) {
        let mut reads = 0;
        let mut compare = |position: [i32; 3], expected| {
            if (0..3).any(|axis| position[axis] < lo[axis] || position[axis] > hi[axis]) {
                return true;
            }
            reads += 1;
            read(position) == expected
        };
        match self {
            Self::Empty => {}
            Self::Uniform(cells, expected) => {
                let low = lo.map(|value| (value + 2).max(0) as usize);
                let high = hi.map(|value| (value + 2).min(19));
                if (0..3).any(|axis| high[axis] < low[axis] as i32) { return (true, 0) }
                let high = high.map(|value| value as usize);
                for y in low[1]..=high[1] {
                    for z in low[2]..=high[2] {
                        let start = low[0] + 20 * (z + 20 * y);
                        let end = high[0] + 20 * (z + 20 * y);
                        for word_index in start / 64..=end / 64 {
                            let first = start.max(word_index * 64) % 64;
                            let length = (end + 1).min((word_index + 1) * 64) - (word_index * 64 + first);
                            let mut bits = cells[word_index] & (((1u64 << length) - 1) << first);
                            while bits != 0 {
                                let index = word_index * 64 + bits.trailing_zeros() as usize;
                                bits &= bits - 1;
                                if !compare(coordinate(index), *expected) { return (false, reads) }
                            }
                        }
                    }
                }
            }
            Self::Sparse(entries) => {
                for &entry in entries {
                    if !compare(coordinate((entry >> 8) as usize), entry as u8) { return (false, reads) }
                }
            }
        }
        (true, reads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_inputs_distinguish_unread_changes_and_changed_values_inside_a_box() {
        let probe = LightReadProbe::with_measurement(false);
        for position in [[-2, 4, 12], [17, 4, 12], [7, 4, 12]] {
            probe.observe(position[0], position[1], position[2], 3, 11);
        }
        let inputs = probe.finish(|_| 0x3B).unwrap();
        assert_eq!(inputs.retained_bytes(), 12);
        assert_eq!(inputs.unchanged([8, 4, 12], [9, 4, 12], |_| panic!("unread cell")), (true, 0));
        assert_eq!(inputs.unchanged([6, 4, 12], [8, 4, 12], |_| 0x3B), (true, 1));
        assert_eq!(inputs.unchanged([6, 4, 12], [8, 4, 12], |_| 0xB3), (false, 1));
        assert_eq!(inputs.unchanged([-2, 4, 12], [-2, 4, 12], |_| 0x3B), (true, 1));
        assert_eq!(inputs.unchanged([17, 4, 12], [17, 4, 12], |_| 0), (false, 1));
        assert_eq!(probe.summary().reads, 0);
    }

    #[test]
    fn light_inputs_bound_uniform_and_mixed_storage_and_do_not_invent_empty_reads() {
        let probe = LightReadProbe::new();
        let empty = probe.finish(|_| panic!("no reads")).unwrap();
        assert_eq!(empty.retained_bytes(), 0);
        assert_eq!(empty.unchanged([-16; 3], [31; 3], |_| panic!("no reads")), (true, 0));
        for index in 0..8000 {
            let [x, y, z] = coordinate(index);
            probe.observe(x, y, z, 15, 0);
        }
        let uniform = probe.finish(|_| panic!("uniform needs no rereads")).unwrap();
        assert_eq!(uniform.retained_bytes(), 1000);
        assert_eq!(uniform.unchanged([0; 3], [15; 3], |_| 0xF0), (true, 4096));
        probe.observe(0, 0, 0, 3, 0);
        assert!(probe.finish(|_| 0).is_none(), "dense mixed inputs must fall back");
        let outside = LightReadProbe::new();
        outside.observe(-3, 0, 0, 15, 0);
        assert!(outside.finish(|_| 0).is_none(), "unknown reads are not an empty witness");
    }

    #[test]
    fn light_inputs_preserve_every_nonwinning_sample() {
        let probe = LightReadProbe::new();
        for (x, sky) in [(0, 3), (1, 11)] { probe.observe(x, 0, 0, sky, 0); }
        let inputs = probe.finish(|[x, _, _]| if x == 0 { 0x30 } else { 0xB0 }).unwrap();
        assert_eq!(inputs.unchanged([0; 3], [0; 3], |_| 0xC0), (false, 1));
    }

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
        for x in -2..=17 {
            for y in -2..=17 {
                for z in -2..=17 {
                    probe.observe(x, y, z, ((x + 2) % 16) as u8, ((z + 2) % 16) as u8);
                }
            }
        }
        assert_eq!(probe.summary(), LightReadSummary {
            distinct_values: 16 * 16,
            unique_cells: 20 * 20 * 20,
            reads: 20 * 20 * 20,
            out_of_domain_reads: 0,
        });
        for [x, y, z] in [
            [-3, 0, 0], [18, 0, 0],
            [0, -3, 0], [0, 18, 0],
            [0, 0, -3], [0, 0, 18],
        ] {
            probe.observe(x, y, z, 15, 0);
        }
        assert_eq!(probe.summary(), LightReadSummary {
            distinct_values: 16 * 16,
            unique_cells: 20 * 20 * 20,
            reads: 20 * 20 * 20 + 6,
            out_of_domain_reads: 6,
        });
    }
}
