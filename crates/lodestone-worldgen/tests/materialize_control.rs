#![allow(unsafe_code)]

use std::hint::black_box;

use lodestone_data::block_states::StateId;
use lodestone_worldgen::dense_grid::DenseBlockGrid;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct RusageInfoV4 {
    _uuid: [u8; 16],
    _user_time: u64,
    _system_time: u64,
    _pkg_idle_wkups: u64,
    _interrupt_wkups: u64,
    _pageins: u64,
    _wired_size: u64,
    _resident_size: u64,
    _phys_footprint: u64,
    _proc_start_abstime: u64,
    _proc_exit_abstime: u64,
    _child_user_time: u64,
    _child_system_time: u64,
    _child_pkg_idle_wkups: u64,
    _child_interrupt_wkups: u64,
    _child_pageins: u64,
    _child_elapsed_abstime: u64,
    _diskio_bytesread: u64,
    _diskio_byteswritten: u64,
    _cpu_time_qos_default: u64,
    _cpu_time_qos_maintenance: u64,
    _cpu_time_qos_background: u64,
    _cpu_time_qos_utility: u64,
    _cpu_time_qos_legacy: u64,
    _cpu_time_qos_user_initiated: u64,
    _cpu_time_qos_user_interactive: u64,
    _billed_system_time: u64,
    _serviced_system_time: u64,
    _logical_writes: u64,
    _lifetime_max_phys_footprint: u64,
    instructions: u64,
    cycles: u64,
    _billed_energy: u64,
    _serviced_energy: u64,
    _interval_max_phys_footprint: u64,
    _runnable_time: u64,
    _flags: u64,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

#[cfg(target_os = "macos")]
fn retired() -> (u64, u64) {
    let mut usage = RusageInfoV4::default();
    let result = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            4,
            (&raw mut usage).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
    (usage.instructions, usage.cycles)
}

#[cfg(not(target_os = "macos"))]
fn retired() -> (u64, u64) {
    (0, 0)
}

fn state(raw: u16) -> StateId {
    StateId::from_raw(raw)
}

#[test]
fn ordered_materialization_control() {
    const SIZE_Y: i32 = 384;
    const CELLS: usize = 16 * SIZE_Y as usize * 16;
    const REPETITIONS: usize = 16;
    let states = [state(0), state(1), state(2), state(3), state(4), state(5)];
    let start = retired();
    let mut digest = 0u64;
    for repetition in 0..REPETITIONS {
        let grid = DenseBlockGrid::from_ordered_state_fn(
            0,
            -64,
            0,
            16,
            SIZE_Y,
            16,
            StateId::AIR,
            |x, y, z| {
                let index = (((y + 64) * 16 + z) * 16 + x) as usize;
                states[(index + repetition) % states.len()]
            },
        );
        let (palette, blocks) = grid.into_id_palette_and_blocks();
        assert_eq!(blocks.len(), CELLS);
        digest = digest
            .wrapping_add(palette.len() as u64)
            .wrapping_add(blocks.iter().map(|&block| block as u64).sum::<u64>());
        black_box((palette, blocks));
    }
    let end = retired();
    println!(
        "MATERIALIZE_CONTROL repetitions={REPETITIONS} cells={} digest={} instructions={} cycles={}",
        CELLS * REPETITIONS,
        digest,
        end.0.saturating_sub(start.0),
        end.1.saturating_sub(start.1)
    );
}

#[test]
fn ordered_packed_materialization_matches_state_builder() {
    let states = [state(0), state(1), state(2), state(3), state(4), state(5)];
    let expected = DenseBlockGrid::from_ordered_state_fn(
        0,
        0,
        0,
        2,
        3,
        1,
        StateId::AIR,
        |x, y, z| states[((z * 2 + x) * 3 + y) as usize],
    );
    let packed = (0..3)
        .flat_map(|y| (0..1).flat_map(move |z| (0..2).map(move |x| states[((z * 2 + x) * 3 + y) as usize].raw() as u16)))
        .collect();
    let actual = DenseBlockGrid::from_ordered_packed_state_fn(
        0,
        0,
        0,
        2,
        3,
        1,
        StateId::AIR,
        packed,
        |_, _, _, _, raw| StateId::from_raw(raw),
    );
    assert_eq!(
        actual.into_id_palette_and_blocks(),
        expected.into_id_palette_and_blocks()
    );
}
