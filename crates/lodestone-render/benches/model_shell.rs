use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{ModelMesh, ModelSectionView, face_of_direction, mesh_models_layers};

#[derive(Clone, Copy, Debug)]
enum Halo { Air, Sealed, Gap }

struct View {
    quads: [BakedQuad; 6],
    halo: Halo,
    boundary_only: bool,
    visits: Cell<usize>,
}

impl ModelSectionView for View {
    fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
        self.visits.set(self.visits.get() + 1);
        &self.quads
    }
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let inside = [x, y, z].iter().all(|v| (0..16).contains(v));
        inside || match self.halo {
            Halo::Air => false,
            Halo::Sealed => true,
            Halo::Gap => (x, y, z) != (16, 7, 11),
        }
    }
    fn interior_quads_are_culled(&self) -> bool { self.boundary_only }
    fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
        ((x * 3 + y * 5 + z * 7).rem_euclid(16) as u8) << 4
    }
}

fn quads() -> [BakedQuad; 6] {
    [Direction::Up, Direction::Down, Direction::North,
        Direction::South, Direction::East, Direction::West].map(|direction| {
        let normal = face_of_direction(direction).normal();
        let axis = normal.iter().position(|&v| v != 0).unwrap();
        let a = (axis + 1) % 3;
        let b = (axis + 2) % 3;
        let positions = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].map(|uv| {
            let mut p = [0.0; 3];
            p[axis] = if normal[axis] > 0 { 1.0 } else { 0.0 };
            p[a] = uv[0];
            p[b] = uv[1];
            p
        });
        BakedQuad { positions, uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction, cullface: Some(direction), tint_index: None, shade: true, shade_direction: None,
            layer: 0, anim: 0, sprite: 0 }
    })
}

fn same_mesh(actual: &ModelMesh, expected: &ModelMesh) {
    assert_eq!(bytemuck::cast_slice::<_, u8>(&actual.vertices),
        bytemuck::cast_slice::<_, u8>(&expected.vertices));
    assert_eq!(actual.indices, expected.indices);
}

fn measure(view: &View, iterations: usize, expected_quads: usize) {
    #[cfg(target_os = "macos")]
    let counters = lodestone_testsupport::process_counters::ProcessCounters::read();
    let started = Instant::now();
    let mut checksum = 0;
    for _ in 0..iterations {
        let mesh = black_box(mesh_models_layers(black_box(view)));
        checksum += mesh.0.quad_count() + mesh.1.quad_count();
    }
    let elapsed = started.elapsed();
    assert_eq!(checksum, expected_quads * iterations);
    #[cfg(target_os = "macos")]
    let counters = counters.and_then(|start| {
        lodestone_testsupport::process_counters::ProcessCounters::read()?.since(start)
    });
    print!("MODEL_SHELL_BENCH halo={:?} boundary_only={} calls={iterations} elapsed_ns={} checksum={checksum}",
        view.halo, view.boundary_only, elapsed.as_nanos());
    #[cfg(target_os = "macos")]
    match counters {
        Ok(counters) => print!(" instructions={} cycles={}", counters.instructions, counters.cycles),
        Err(error) => print!(" counters_unavailable={error:?}"),
    }
    println!();
}

fn main() {
    let test = std::env::args().any(|arg| arg == "--test");
    let full_only = std::env::args().any(|arg| arg == "--full-only");
    let iterations = if test { 1 } else { 256 };
    for (halo, expected_quads) in [(Halo::Air, 6 * 16 * 16), (Halo::Sealed, 0), (Halo::Gap, 1)] {
        let mut view = View { quads: quads(), halo, boundary_only: false, visits: Cell::new(0) };
        let full = mesh_models_layers(&view);
        assert_eq!(view.visits.replace(0), 4096);
        if full_only {
            assert_eq!(full.0.quad_count() + full.1.quad_count(), expected_quads);
            for _ in 0..if test { 1 } else { 5 } {
                measure(&view, iterations, expected_quads);
            }
            continue;
        }
        view.boundary_only = true;
        let shell = mesh_models_layers(&view);
        assert_eq!(view.visits.replace(0), 1352);
        assert_eq!(shell.0.quad_count() + shell.1.quad_count(), expected_quads);
        same_mesh(&shell.0, &full.0);
        same_mesh(&shell.1, &full.1);
        println!("MODEL_SHELL_CONTROL halo={halo:?} full_cells=4096 shell_cells=1352 quads={expected_quads} exact_geometry=true");
        for round in 0..if test { 1 } else { 5 } {
            for boundary_only in if round % 2 == 0 { [false, true] } else { [true, false] } {
                view.boundary_only = boundary_only;
                measure(&view, iterations, expected_quads);
            }
        }
    }
}
