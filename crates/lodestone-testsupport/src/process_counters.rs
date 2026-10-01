//! Process-wide retired counters for isolated macOS benchmark arms.

use std::io;
use std::mem::{MaybeUninit, size_of};

// The macOS SDK's v4 record ends at ri_runnable_time; ri_flags belongs to v5.
const _: () = assert!(size_of::<libc::rusage_info_v4>() == 296);

#[derive(Clone, Copy, Debug)]
pub struct ProcessCounters {
    pub instructions: u64,
    pub cycles: u64,
}

impl ProcessCounters {
    /// Reads the current process totals. Concurrent work is included.
    #[allow(unsafe_code)]
    pub fn read() -> io::Result<Self> {
        let pid = i32::try_from(std::process::id())
            .map_err(|_| io::Error::other("process id does not fit the macOS API"))?;
        let mut usage = MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: the v4 call writes a correctly sized, aligned record for our pid.
        // Its pointer parameter names an output buffer, not a pointer to dereference.
        let usage = unsafe {
            let result = libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, usage.as_mut_ptr().cast());
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            usage.assume_init()
        };
        if usage.ri_instructions == 0 || usage.ri_cycles == 0 {
            return Err(io::Error::new(io::ErrorKind::Unsupported,
                "macOS returned unavailable retired instruction or cycle counters"));
        }
        Ok(Self { instructions: usage.ri_instructions, cycles: usage.ri_cycles })
    }

    /// Subtracts an earlier snapshot, rejecting a counter that moved backwards.
    pub fn since(self, earlier: Self) -> io::Result<Self> {
        let instructions = self.instructions.checked_sub(earlier.instructions);
        let cycles = self.cycles.checked_sub(earlier.cycles);
        match (instructions, cycles) {
            (Some(instructions), Some(cycles)) => Ok(Self { instructions, cycles }),
            _ => Err(io::Error::other("macOS process counters moved backwards")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessCounters;
    use std::hint::black_box;

    #[inline(never)]
    fn arithmetic(iterations: u64) {
        let mut value = black_box(7_u64);
        for i in 0..black_box(iterations) {
            value = value.rotate_left(5).wrapping_add(i).wrapping_mul(0x9e3779b97f4a7c15);
        }
        black_box(value);
    }

    fn measure(iterations: u64) -> ProcessCounters {
        let started = ProcessCounters::read().expect("retired counters available");
        arithmetic(iterations);
        ProcessCounters::read().expect("retired counters available")
            .since(started).expect("monotonic counters")
    }

    #[test]
    #[ignore = "process-wide calibration requires an isolated test process"]
    fn retired_counter_distinguishes_noop_and_fourfold_arithmetic() {
        arithmetic(10_000);
        let noop = measure(0);
        let short = measure(100_000);
        let long = measure(400_000);
        assert!(short.instructions > noop.instructions * 8,
            "no-op={noop:?}, short={short:?}, long={long:?}");
        let ratio = (long.instructions - noop.instructions) as f64
            / (short.instructions - noop.instructions) as f64;
        assert!((3.7..=4.3).contains(&ratio),
            "predicted 4x retired instructions: ratio={ratio}, no-op={noop:?}, short={short:?}, long={long:?}");
        eprintln!("PROCESS_COUNTER_CONTROL noop={noop:?} short={short:?} long={long:?} ratio={ratio:.3}");
    }
}
