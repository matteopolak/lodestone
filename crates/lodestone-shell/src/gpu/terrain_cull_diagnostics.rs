//! Opt-in, edge-triggered evidence for angle-dependent terrain and sign failures.
//!
//! Enable with `RUST_LOG=terrain_cull=debug`. The line is emitted only when a
//! sampled section's cull verdict, an aggregate cull count, or the sign upload
//! outcome changes; it therefore remains useful while sweeping the camera rather
//! than printing once per rendered frame.

use lodestone_render::{Camera, CullVerdict, SectionCoord, TerrainCull, section_coord_of};

use super::RenderStats;

const TARGET: &str = "terrain_cull";

/// Counts produced by `SignTextRenderer::prepare`, kept separate from its
/// vertex count so a line distinguishes an empty gather, behind-eye rejection,
/// a budget drop, and a healthy upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SignPrepareCounts {
    pub(super) gathered: usize,
    pub(super) in_front: usize,
    pub(super) drawn: usize,
    pub(super) vertices: u32,
    pub(super) capacity: usize,
}

/// The persistent, per-renderer edge latch. Its optional target comes from
/// `LODESTONE_TERRAIN_CULL_PROBE_SECTION=x,y,z`; otherwise the camera section
/// is sampled, which makes a zero-config reproduction immediately useful.
#[derive(Debug, Default)]
pub(super) struct Probe {
    target: Option<SectionCoord>,
    last: Option<Signal>,
}

impl Probe {
    #[must_use]
    pub(super) fn from_environment() -> Self {
        Self {
            target: std::env::var("LODESTONE_TERRAIN_CULL_PROBE_SECTION")
                .ok()
                .as_deref()
                .and_then(parse_section),
            last: None,
        }
    }

    pub(super) fn report(
        &mut self,
        camera: &Camera,
        cull: &TerrainCull,
        stats: RenderStats,
        signs: SignPrepareCounts,
    ) {
        if !tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
            return;
        }
        let target = self.target.unwrap_or_else(|| section_coord_of(camera.position));
        let nearby_coords = neighbourhood(target);
        let nearby_verdicts = nearby_coords.map(|coord| cull.classify(coord));
        let signal = Signal {
            target,
            target_verdict: cull.classify(target),
            neighbourhood: nearby_verdicts,
            sections_drawn: stats.sections_drawn,
            culled_frustum: stats.sections_culled_frustum,
            culled_occlusion: stats.sections_culled_occlusion,
            occlusion_shadow: stats.sections_occlusion_shadow,
            sign_gathered: signs.gathered,
            sign_in_front: signs.in_front,
            sign_drawn: signs.drawn,
            sign_vertices: signs.vertices,
        };
        if !self.changed(signal) {
            return;
        }

        let nearby = nearby_coords
            .into_iter()
            .zip(nearby_verdicts)
            .map(|((x, y, z), verdict)| format!("{x},{y},{z}={verdict:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::debug!(
            target: TARGET,
            "camera=({:.2},{:.2},{:.2}) yaw={:.1} pitch={:.1}; target_section={},{},{} verdict={:?}; around=[{}]; terrain drawn={} frustum={} occlusion={} shadow={}; signs gathered={} in_front={} drawn={} vertices={}/{}",
            camera.position.x,
            camera.position.y,
            camera.position.z,
            camera.yaw,
            camera.pitch,
            target.0,
            target.1,
            target.2,
            signal.target_verdict,
            nearby,
            signal.sections_drawn,
            signal.culled_frustum,
            signal.culled_occlusion,
            signal.occlusion_shadow,
            signal.sign_gathered,
            signal.sign_in_front,
            signal.sign_drawn,
            signal.sign_vertices,
            signs.capacity,
        );
    }

    fn changed(&mut self, next: Signal) -> bool {
        let changed = self.last != Some(next);
        self.last = Some(next);
        changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Signal {
    target: SectionCoord,
    target_verdict: CullVerdict,
    neighbourhood: [CullVerdict; 9],
    sections_drawn: usize,
    culled_frustum: usize,
    culled_occlusion: usize,
    occlusion_shadow: usize,
    sign_gathered: usize,
    sign_in_front: usize,
    sign_drawn: usize,
    sign_vertices: u32,
}

fn neighbourhood((x, y, z): SectionCoord) -> [SectionCoord; 9] {
    [
        (x - 1, y, z - 1), (x, y, z - 1), (x + 1, y, z - 1),
        (x - 1, y, z),     (x, y, z),     (x + 1, y, z),
        (x - 1, y, z + 1), (x, y, z + 1), (x + 1, y, z + 1),
    ]
}

fn parse_section(value: &str) -> Option<SectionCoord> {
    let mut fields = value.split(',').map(str::trim);
    let x = fields.next()?.parse().ok()?;
    let y = fields.next()?.parse().ok()?;
    let z = fields.next()?.parse().ok()?;
    fields.next().is_none().then_some((x, y, z))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(sign_drawn: usize) -> Signal {
        Signal {
            target: (1, 2, 3),
            target_verdict: CullVerdict::Visible,
            neighbourhood: [CullVerdict::Visible; 9],
            sections_drawn: 10,
            culled_frustum: 4,
            culled_occlusion: 0,
            occlusion_shadow: 0,
            sign_gathered: 2,
            sign_in_front: 2,
            sign_drawn,
            sign_vertices: 48,
        }
    }

    #[test]
    fn logs_only_when_a_relevant_cull_or_sign_signal_changes() {
        let mut probe = Probe::default();
        let stable = signal(2);
        assert!(probe.changed(stable));
        assert!(!probe.changed(stable));
        assert!(probe.changed(signal(1)));
    }

    #[test]
    fn parses_only_a_complete_section_coordinate() {
        assert_eq!(parse_section("-2, 4, 8"), Some((-2, 4, 8)));
        assert_eq!(parse_section("-2,4"), None);
        assert_eq!(parse_section("-2,4,8,16"), None);
    }
}
