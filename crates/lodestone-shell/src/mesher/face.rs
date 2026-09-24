//! Packed full-cube face emission over a section snapshot.
use super::*;

/// Mesh a snapshot into geometry. Pure and thread-safe: touches only the owned
/// snapshot and a stateless classifier. Generic over the [`BlockClassifier`] so
/// the same code meshes the demo world (via [`crate::blocks::DemoClassifier`])
/// and the live vanilla world (via a [`crate::blocks::ShellClassifier::Vanilla`]
/// atlas) without duplication.
#[must_use]
pub fn mesh_snapshot<C: BlockClassifier>(snapshot: &SectionSnapshot, classifier: &C) -> Mesh {
    // Real per-section light, replacing the retired full-bright bridge. The
    // packed path lights each *cell* and lets `mesh_simple` sample the
    // neighbouring cell per face itself, so it needs the raw per-slot sources
    // rather than `SnapshotLight`'s face rule.
    let srcs = SnapshotLight::new(snapshot).slots;

    // Build a view per neighbour section, then assemble the neighbourhood.
    let mut views: Vec<ChunkSectionView<'_, C, SnapLight<'_>>> = Vec::with_capacity(27);
    let mut i = 0usize;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                views.push(ChunkSectionView::new(
                    snapshot.at(dx, dy, dz),
                    classifier,
                    &srcs[i],
                ));
                i += 1;
            }
        }
    }
    let idx = |dx: i32, dy: i32, dz: i32| ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;

    let mut hood = SectionNeighborhood::centre_only(&views[idx(0, 0, 0)]);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                hood.set(dx, dy, dz, Some(&views[idx(dx, dy, dz)]));
            }
        }
    }

    mesh_simple(&hood)
}
