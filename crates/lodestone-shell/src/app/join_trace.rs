use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Duration};

use crate::{menu::loading::ConnectPhase, platform::Instant};

#[derive(Clone, Debug)]
pub struct BrowserJoinProgress {
    pub phase: &'static str,
    pub elapsed_ms: f64,
    pub loaded_columns: usize,
    pub expected_columns: usize,
    pub settled_columns: usize,
    pub pending_meshes: usize,
    pub pending_light_remeshes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct JoinMeshWork {
    pub pending_meshes: usize,
    pub pending_columns: usize,
    pub pending_light_remeshes: usize,
    pub pending_removals: usize,
}

impl JoinMeshWork {
    #[cfg(target_arch = "wasm32")]
    pub(super) fn sample(sim: &crate::sim::Sim) -> Self {
        lodestone_ecs::hold_read(sim.ecs(), |world| {
            let terrain = world.resource::<crate::mesher::TerrainMesh>();
            let backlog = terrain.backlog();
            Self {
                pending_meshes: backlog.pending_sections,
                pending_columns: backlog.ready_columns + backlog.forced_columns,
                pending_light_remeshes: terrain.light_dirty_sections.len(),
                pending_removals: terrain.pending_removals.len(),
            }
        })
    }

    fn is_drained(self) -> bool {
        self.pending_meshes == 0
            && self.pending_columns == 0
            && self.pending_light_remeshes == 0
            && self.pending_removals == 0
    }
}

#[derive(Debug)]
pub(super) struct BrowserJoinTrace {
    started: Option<Instant>,
    expected_columns: usize,
    joining: bool,
    loading_terrain: bool,
    overlay_ready: bool,
    first_terrain_presented: bool,
    gameplay_ready: bool,
    full_view_presented: bool,
    full_view_quiescent: bool,
    last_full_view_probe: Option<Instant>,
    #[cfg(target_arch = "wasm32")]
    last_view_report: Option<Instant>,
    events: Rc<RefCell<VecDeque<BrowserJoinProgress>>>,
}

impl BrowserJoinTrace {
    pub(super) fn new(events: Rc<RefCell<VecDeque<BrowserJoinProgress>>>) -> Self {
        Self {
            started: None,
            expected_columns: 0,
            joining: false,
            loading_terrain: false,
            overlay_ready: false,
            first_terrain_presented: false,
            gameplay_ready: false,
            full_view_presented: false,
            full_view_quiescent: false,
            last_full_view_probe: None,
            #[cfg(target_arch = "wasm32")]
            last_view_report: None,
            events,
        }
    }

    pub(super) fn start(&mut self, radius: u32, phase: &'static str) {
        self.started = Some(Instant::now());
        self.expected_columns = crate::menu::loading::TerrainProgress::expected_for_radius(radius);
        self.joining = false;
        self.loading_terrain = false;
        self.overlay_ready = false;
        self.first_terrain_presented = false;
        self.gameplay_ready = false;
        self.full_view_presented = false;
        self.full_view_quiescent = false;
        self.last_full_view_probe = None;
        #[cfg(target_arch = "wasm32")]
        {
            self.last_view_report = None;
        }
        self.events.borrow_mut().clear();
        self.push(phase, 0, 0, JoinMeshWork::default());
    }

    pub(super) fn observe_before_present(
        &mut self,
        phase: ConnectPhase,
        overlay_ready: bool,
        loaded_columns: usize,
        work: JoinMeshWork,
    ) {
        if !self.joining && phase == ConnectPhase::Joining {
            self.joining = true;
            self.push("joining", loaded_columns, 0, work);
        }
        if !self.loading_terrain && phase == ConnectPhase::LoadingTerrain {
            self.loading_terrain = true;
            self.push("loading-terrain", loaded_columns, 0, work);
        }
        if !self.overlay_ready && overlay_ready {
            self.overlay_ready = true;
            self.push("loading-overlay-ready", loaded_columns, 0, work);
        }
    }

    pub(super) fn observe_presented(
        &mut self,
        terrain_drawn: bool,
        loaded_columns: usize,
        view_presentation: Option<(usize, usize, usize)>,
        view_settlement: Option<(usize, usize, usize)>,
        work: JoinMeshWork,
    ) {
        if !self.first_terrain_presented && terrain_drawn {
            self.first_terrain_presented = true;
            self.push(
                "first-terrain-presented",
                loaded_columns,
                view_presentation.map_or(0, |(_, presented, _)| presented),
                work,
            );
        }
        if let Some((resident, presented, expected)) = view_presentation {
            self.expected_columns = expected;
            #[cfg(target_arch = "wasm32")]
            if log::max_level() >= log::LevelFilter::Debug
                && self.last_view_report.is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
            {
                self.last_view_report = Some(Instant::now());
                let settled = view_settlement.map_or(0, |(_, settled, _)| settled);
                crate::net::browser_diagnostic(format_args!(
                    "view presentation: resident={resident} presented={presented} expected={expected} missing_resident={} resident_unpresented={} settled={settled} submitted_meshes={} pending_light_remeshes={} pending_columns={} pending_removals={}",
                    expected.saturating_sub(resident),
                    resident.saturating_sub(presented),
                    work.pending_meshes,
                    work.pending_light_remeshes,
                    work.pending_columns,
                    work.pending_removals,
                ));
            }
            let full_view_presented = expected > 0 && resident == expected && presented == expected;
            if !self.full_view_presented && full_view_presented {
                self.full_view_presented = true;
                self.push("full-view-presented", resident, presented, work);
            }
            if !self.full_view_quiescent
                && full_view_presented
                && view_settlement == Some((expected, expected, expected))
                && work.is_drained()
            {
                self.full_view_quiescent = true;
                self.push("full-view-quiescent", resident, expected, work);
            }
        }
    }

    pub(super) fn observe_gameplay_presented(
        &mut self,
        ready: bool,
        terrain_drawn: bool,
        loaded_columns: usize,
        work: JoinMeshWork,
    ) {
        if !self.gameplay_ready && ready && terrain_drawn {
            self.gameplay_ready = true;
            self.push("gameplay-ready", loaded_columns, 0, work);
        }
    }

    pub(super) fn full_view_probe_due(&mut self) -> bool {
        self.full_view_probe_due_at(Instant::now())
    }

    fn full_view_probe_due_at(&mut self, now: Instant) -> bool {
        if self.started.is_none() {
            return false;
        }
        let interval = if self.full_view_quiescent {
            if log::max_level() < log::LevelFilter::Debug {
                return false;
            }
            Duration::from_secs(1)
        } else {
            Duration::from_millis(100)
        };
        if self.last_full_view_probe.is_some_and(|last| now.duration_since(last) < interval) {
            return false;
        }
        self.last_full_view_probe = Some(now);
        true
    }

    fn push(
        &self,
        phase: &'static str,
        loaded_columns: usize,
        settled_columns: usize,
        work: JoinMeshWork,
    ) {
        let Some(started) = self.started else {
            return;
        };
        let mut events = self.events.borrow_mut();
        if events.len() == 16 {
            events.pop_front();
        }
        events.push_back(BrowserJoinProgress {
            phase,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            loaded_columns,
            expected_columns: self.expected_columns,
            settled_columns,
            pending_meshes: work.pending_meshes,
            pending_light_remeshes: work.pending_light_remeshes,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> (BrowserJoinTrace, Rc<RefCell<VecDeque<BrowserJoinProgress>>>) {
        let events = Rc::new(RefCell::new(VecDeque::new()));
        (BrowserJoinTrace::new(Rc::clone(&events)), events)
    }

    #[test]
    fn gameplay_readiness_requires_input_and_terrain_and_resets_between_joins() {
        let (mut trace, events) = trace();
        let work = JoinMeshWork::default();
        trace.start(2, "world-create-started");
        trace.observe_gameplay_presented(false, true, 1, work);
        trace.observe_gameplay_presented(true, false, 1, work);
        assert_eq!(events.borrow().len(), 1);
        trace.observe_gameplay_presented(true, true, 1, work);
        trace.observe_gameplay_presented(true, true, 1, work);
        assert_eq!(events.borrow().len(), 2);
        assert_eq!(events.borrow().back().unwrap().phase, "gameplay-ready");
        trace.start(2, "world-open-started");
        assert!(!trace.gameplay_ready);
        trace.observe_gameplay_presented(true, true, 1, work);
        assert_eq!(events.borrow().len(), 2);
    }

    #[test]
    fn browser_join_quiescence_waits_for_905_pending_meshes_and_latest_uploads() {
        let (mut trace, events) = trace();
        trace.start(2, "world-create-started");
        trace.started = Some(Instant::now() - Duration::from_secs(17));
        let view = Some((25, 25, 25));
        trace.observe_presented(true, 25, view, view, JoinMeshWork {
            pending_meshes: 905,
            ..JoinMeshWork::default()
        });
        assert_eq!(events.borrow().back().unwrap().phase, "full-view-presented");
        assert_eq!(events.borrow().back().unwrap().pending_meshes, 905);
        assert!(!trace.full_view_quiescent);
        trace.observe_presented(true, 25, view, Some((25, 24, 25)), JoinMeshWork::default());
        assert!(!trace.full_view_quiescent, "latest upload handoff is still missing");
        trace.observe_presented(true, 25, view, view, JoinMeshWork::default());
        trace.observe_presented(true, 25, view, view, JoinMeshWork::default());
        let events = events.borrow();
        let settled: Vec<_> = events.iter()
            .filter(|event| event.phase == "full-view-quiescent").collect();
        assert_eq!(settled.len(), 1);
        assert!(settled[0].elapsed_ms >= 17_000.0);
        assert_eq!(settled[0].loaded_columns, 25);
        assert_eq!(settled[0].expected_columns, 25);
        assert_eq!(settled[0].settled_columns, 25);
        assert_eq!(settled[0].pending_meshes, 0);
        assert_eq!(settled[0].pending_light_remeshes, 0);
    }

    #[test]
    fn browser_join_quiescence_waits_for_each_work_queue_and_current_view() {
        let view = Some((25, 25, 25));
        for work in [
            JoinMeshWork { pending_meshes: 1, ..JoinMeshWork::default() },
            JoinMeshWork { pending_columns: 1, ..JoinMeshWork::default() },
            JoinMeshWork { pending_light_remeshes: 905, ..JoinMeshWork::default() },
            JoinMeshWork { pending_removals: 1, ..JoinMeshWork::default() },
        ] {
            let (mut trace, events) = trace();
            trace.start(2, "world-create-started");
            trace.observe_presented(true, 25, view, view, work);
            assert!(!trace.full_view_quiescent, "work remains: {work:?}");
            assert_eq!(
                events.borrow().back().unwrap().pending_light_remeshes,
                work.pending_light_remeshes,
            );
            trace.observe_presented(true, 25, None, view, JoinMeshWork::default());
            assert!(!trace.full_view_quiescent, "presentation was not probed");
            trace.observe_presented(true, 25, Some((24, 24, 25)), view, JoinMeshWork::default());
            assert!(
                !trace.full_view_quiescent,
                "previous presentation cannot cover a changed view",
            );
            trace.observe_presented(true, 25, view, view, JoinMeshWork::default());
            assert!(trace.full_view_quiescent);
        }
    }

    #[test]
    fn browser_join_quiescence_resets_and_keeps_probing_after_first_full_view() {
        let (mut trace, events) = trace();
        assert!(!trace.full_view_probe_due());
        trace.start(2, "world-create-started");
        let view = Some((25, 25, 25));
        trace.observe_before_present(ConnectPhase::LoadingTerrain, true, 25, JoinMeshWork::default());
        trace.observe_presented(true, 25, view, view, JoinMeshWork {
            pending_meshes: 905,
            ..JoinMeshWork::default()
        });
        let now = Instant::now();
        assert!(trace.full_view_probe_due_at(now));
        assert!(!trace.full_view_probe_due_at(now + Duration::from_millis(99)));
        assert!(trace.full_view_probe_due_at(now + Duration::from_millis(100)));
        trace.observe_presented(true, 25, view, view, JoinMeshWork::default());
        assert!(trace.full_view_quiescent);
        trace.start(1, "world-open-started");
        assert!(!trace.full_view_quiescent);
        assert!(!trace.overlay_ready);
        assert!(!trace.full_view_presented);
        assert_eq!(events.borrow().len(), 1);
        assert_eq!(events.borrow().front().unwrap().phase, "world-open-started");
        assert!(trace.full_view_probe_due());
        let view = Some((9, 9, 9));
        trace.observe_presented(true, 9, view, view, JoinMeshWork::default());
        assert_eq!(events.borrow().back().unwrap().phase, "full-view-quiescent");
        assert_eq!(events.borrow().back().unwrap().expected_columns, 9);
    }
}
