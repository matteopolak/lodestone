use std::hint::black_box;
use std::time::Instant;

use lodestone_render::section::Face;
use lodestone_render::visibility::{SectionVisibility, compute_visibility_from};

#[derive(Clone, Copy, Debug)]
enum Scene { Empty, Solid, Wall, WallGap, Tube, Surface, Checkerboard, Pocket }

impl Scene {
    fn opaque(self, x: usize, y: usize, z: usize) -> bool {
        match self {
            Self::Empty => false,
            Self::Solid => true,
            Self::Wall => x == 8,
            Self::WallGap => x == 8 && (y != 7 || z != 11),
            Self::Tube => x != 8 || y != 8,
            Self::Surface => y < 8,
            Self::Checkerboard => (x + y + z) % 2 == 0,
            Self::Pocket => !(7..=8).contains(&x) || !(7..=8).contains(&y) || !(7..=8).contains(&z),
        }
    }

    fn connects(self, a: Face, b: Face) -> bool {
        if a == b { return true; }
        match self {
            Self::Empty | Self::WallGap => true,
            Self::Solid | Self::Pocket => false,
            Self::Wall => !matches!((a, b), (Face::NegX, Face::PosX) | (Face::PosX, Face::NegX)),
            Self::Tube => matches!((a, b), (Face::NegZ, Face::PosZ) | (Face::PosZ, Face::NegZ)),
            Self::Surface => a != Face::NegY && b != Face::NegY,
            Self::Checkerboard => a.index() / 2 != b.index() / 2,
        }
    }
}

fn mask(visibility: SectionVisibility) -> u64 {
    let mut mask = 0;
    for a in Face::ALL {
        for b in Face::ALL {
            mask |= u64::from(visibility.connects(a, b)) << (a.index() * 6 + b.index());
        }
    }
    mask
}

fn main() {
    let test = std::env::args().any(|arg| arg == "--test");
    let iterations = if test { 1 } else { 256 };
    for scene in [Scene::Empty, Scene::Solid, Scene::Wall, Scene::WallGap,
        Scene::Tube, Scene::Surface, Scene::Checkerboard, Scene::Pocket]
    {
        let mut cells = [false; 4096];
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 { cells[(x * 16 + y) * 16 + z] = scene.opaque(x, y, z); }
            }
        }
        let reads = std::cell::Cell::new(0_u64);
        let result = compute_visibility_from(|x, y, z| {
            reads.set(reads.get() + 1);
            cells[(x * 16 + y) * 16 + z]
        });
        for a in Face::ALL {
            for b in Face::ALL {
                assert_eq!(result.connects(a, b), scene.connects(a, b), "{scene:?} {a:?}->{b:?}");
            }
        }
        println!("VISIBILITY_CONTROL scene={scene:?} predicate_reads={} mask={:x}", reads.get(), mask(result));
        for _ in 0..if test { 1 } else { 5 } {
            #[cfg(target_os = "macos")]
            let counters = lodestone_testsupport::process_counters::ProcessCounters::read();
            let started = Instant::now();
            let mut checksum = 0_u64;
            for _ in 0..iterations {
                let visibility = compute_visibility_from(|x, y, z| black_box(&cells)[(x * 16 + y) * 16 + z]);
                checksum = checksum.wrapping_add(mask(black_box(visibility)));
            }
            let elapsed = started.elapsed();
            #[cfg(target_os = "macos")]
            let counters = counters.and_then(|start| {
                lodestone_testsupport::process_counters::ProcessCounters::read()?.since(start)
            });
            assert_eq!(checksum, mask(result) * iterations);
            print!("VISIBILITY_BENCH scene={scene:?} calls={iterations} elapsed_ns={} checksum={checksum}", elapsed.as_nanos());
            #[cfg(target_os = "macos")]
            match counters {
                Ok(counters) => print!(" instructions={} cycles={}", counters.instructions, counters.cycles),
                Err(error) => print!(" counters_unavailable={error:?}"),
            }
            println!();
        }
    }
}
