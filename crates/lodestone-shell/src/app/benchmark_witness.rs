use serde_json::{Value, json};

use crate::platform::Instant;

pub(crate) const MAX_ROWS: usize = 2_048;
const CADENCE_MS: u64 = 100;
const PHASE_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WitnessPhase {
    WaitingForJoin,
    Warmup,
    Mutation,
    Stationary,
    Moving,
    Complete,
}

impl WitnessPhase {
    fn name(self) -> &'static str {
        match self {
            Self::WaitingForJoin => "waiting_for_join",
            Self::Warmup => "warmup",
            Self::Mutation => "mutation",
            Self::Stationary => "stationary",
            Self::Moving => "moving",
            Self::Complete => "complete",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CameraWitness {
    pub eye: [f64; 3],
    pub yaw_degrees: f64,
    pub pitch_degrees: f64,
    pub fov_y_degrees: f64,
    pub aspect: f64,
    pub near: f64,
    pub far: f64,
}

impl From<&lodestone_render::Camera> for CameraWitness {
    fn from(camera: &lodestone_render::Camera) -> Self {
        Self {
            eye: camera.position.to_array().map(f64::from),
            yaw_degrees: camera.yaw.into(),
            pitch_degrees: camera.pitch.into(),
            fov_y_degrees: camera.fov_y_degrees.into(),
            aspect: camera.aspect.into(),
            near: camera.near.into(),
            far: camera.far.into(),
        }
    }
}

impl CameraWitness {
    fn scalars(self) -> [f64; 9] {
        [self.eye[0], self.eye[1], self.eye[2], wrap_degrees(self.yaw_degrees),
            self.pitch_degrees, self.fov_y_degrees, self.aspect, self.near, self.far]
    }

    fn finite(self) -> bool {
        self.scalars().into_iter().all(f64::is_finite)
    }

    fn json(self) -> Value {
        json!({"eye": self.eye, "yaw_degrees": self.yaw_degrees,
            "pitch_degrees": self.pitch_degrees, "fov_y_degrees": self.fov_y_degrees,
            "aspect": self.aspect, "near": self.near, "far": self.far})
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ForegroundWitness {
    pub focused: bool,
    pub visible: bool,
    pub focus_observed: bool,
    pub visibility_observed: bool,
    pub idle_seconds: f64,
    pub effective_cap: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AttemptWitness {
    pub framebuffer: [u32; 2],
    pub foreground: ForegroundWitness,
    pub session_connected: bool,
    pub loading: bool,
    pub input_ready: bool,
    pub player_feet: [f64; 3],
    pub player_yaw_degrees: f64,
    pub player_pitch_degrees: f64,
    pub settings_match: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CenterSource {
    Server,
    PlayerFallback,
}

impl CenterSource {
    fn name(self) -> &'static str {
        match self { Self::Server => "server", Self::PlayerFallback => "player_fallback" }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CoverageWitness {
    pub resident: usize,
    pub settled: usize,
    pub expected: usize,
}

impl CoverageWitness {
    fn json(self) -> Value {
        json!({"resident": self.resident, "settled": self.settled, "expected": self.expected})
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ViewWitness {
    pub dimension: Option<String>,
    pub simulation_distance: Option<i32>,
    pub requested_radius: u32,
    pub declared_radius: Option<u32>,
    pub center: Option<[i32; 2]>,
    pub center_source: Option<CenterSource>,
    pub square_diagnostic: Option<CoverageWitness>,
    pub domain_id: Option<String>,
    pub resident_domain: Option<(usize, usize)>,
    pub render_domain: Option<CoverageWitness>,
    pub mesh_backlog: usize,
    pub pending_meshes: usize,
}

#[derive(Debug, Clone)]
pub struct WitnessHeader {
    pub metadata: Value,
    pub expected_settings: Option<crate::config::BenchmarkGraphicsSettings>,
    pub requested_framebuffer: [u32; 2],
    pub requested_camera: Option<CameraWitness>,
    pub requested_radius: u32,
    pub expected_declared_radius: u32,
    pub expected_dimension: Option<String>,
    pub expected_simulation_distance: Option<i32>,
    pub expected_center: Option<[i32; 2]>,
    pub expected_domain_id: Option<String>,
    pub expected_domain: Vec<[i32; 2]>,
    pub expected_render_domain: Vec<[i32; 2]>,
    pub phase_durations_ms: [Option<u64>; PHASE_COUNT],
}

impl WitnessHeader {
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("witness must be an object")?;
        let allowed = ["metadata", "expected_settings", "requested_framebuffer", "requested_camera", "requested_radius",
            "expected_dimension", "expected_simulation_distance", "expected_center",
            "expected_domain_id", "expected_domain", "expected_render_domain",
            "expected_declared_radius", "phase_durations_ms"];
        if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(format!("unknown witness field: {key}"));
        }
        let get = |key: &str| object.get(key).ok_or_else(|| format!("missing witness field: {key}"));
        let array = |key: &str, length: usize| -> Result<&Vec<Value>, String> {
            get(key)?.as_array().filter(|values| values.len() == length)
                .ok_or_else(|| format!("witness.{key} must have {length} elements"))
        };
        let unsigned = |value: &Value| value.as_u64().and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| "witness requires unsigned 32-bit integers".to_owned());
        let signed = |value: &Value| value.as_i64().and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| "witness requires signed 32-bit integers".to_owned());
        let nonempty = |key: &str| -> Result<String, String> {
            get(key)?.as_str().filter(|v| !v.is_empty() && v.len() <= 1_024)
                .map(str::to_owned).ok_or_else(|| format!("witness.{key} must be a nonempty string"))
        };
        let size = array("requested_framebuffer", 2)?;
        let requested_framebuffer = [unsigned(&size[0])?, unsigned(&size[1])?];
        if requested_framebuffer.contains(&0) { return Err("witness framebuffer must be nonzero".into()); }
        let metadata = get("metadata")?;
        if !metadata.is_object() || metadata.to_string().len() > 65_536 {
            return Err("witness.metadata must be an object of at most 65536 encoded bytes".into());
        }
        let center = array("expected_center", 2)?;
        let coordinates = get("expected_domain")?.as_array()
            .filter(|values| !values.is_empty() && values.len() <= 4_096)
            .ok_or("witness.expected_domain must contain 1..=4096 positions")?;
        let mut expected_domain = Vec::with_capacity(coordinates.len());
        let mut unique = std::collections::HashSet::with_capacity(coordinates.len());
        for value in coordinates {
            let pair = value.as_array().filter(|v| v.len() == 2)
                .ok_or("witness domain positions must be [chunk_x,chunk_z]")?;
            let position = [signed(&pair[0])?, signed(&pair[1])?];
            if !unique.insert(position) { return Err("witness domain positions must be unique".into()); }
            expected_domain.push(position);
        }
        let coordinates = get("expected_render_domain")?.as_array()
            .filter(|values| !values.is_empty() && values.len() <= 4_096)
            .ok_or("witness.expected_render_domain must contain 1..=4096 positions")?;
        let mut expected_render_domain = Vec::with_capacity(coordinates.len());
        let mut render_unique = std::collections::HashSet::with_capacity(coordinates.len());
        for value in coordinates {
            let pair = value.as_array().filter(|v| v.len() == 2)
                .ok_or("witness render-domain positions must be [chunk_x,chunk_z]")?;
            let position = [signed(&pair[0])?, signed(&pair[1])?];
            if !unique.contains(&position) || !render_unique.insert(position) {
                return Err("witness render domain must be a unique subset of the resident domain".into());
            }
            expected_render_domain.push(position);
        }
        let camera = get("requested_camera")?.as_object().ok_or("witness.requested_camera must be an object")?;
        let scalar = |key: &str| camera.get(key).and_then(Value::as_f64).filter(|v| v.is_finite())
            .ok_or_else(|| format!("witness.requested_camera.{key} must be finite"));
        let eye = camera.get("eye").and_then(Value::as_array).filter(|v| v.len() == 3)
            .ok_or("witness.requested_camera.eye must have three coordinates")?;
        let mut requested_eye = [0.0; 3];
        for (target, value) in requested_eye.iter_mut().zip(eye) {
            *target = value.as_f64().filter(|v| v.is_finite()).ok_or("witness camera eye must be finite")?;
        }
        let requested_camera = CameraWitness { eye: requested_eye,
            yaw_degrees: scalar("yaw_degrees")?, pitch_degrees: scalar("pitch_degrees")?,
            fov_y_degrees: scalar("fov_y_degrees")?, aspect: scalar("aspect")?,
            near: scalar("near")?, far: scalar("far")? };
        if !(0.0..180.0).contains(&requested_camera.fov_y_degrees)
            || requested_camera.fov_y_degrees == 0.0 || requested_camera.aspect <= 0.0
            || requested_camera.near <= 0.0 || requested_camera.far <= requested_camera.near {
            return Err("witness camera projection must have valid FOV, aspect and clip planes".into());
        }
        let mut phase_durations_ms = [None; PHASE_COUNT];
        if let Some(value) = object.get("phase_durations_ms") {
            let values = value.as_array().filter(|v| v.len() == PHASE_COUNT)
                .ok_or("witness.phase_durations_ms must have five elements")?;
            for (target, value) in phase_durations_ms.iter_mut().zip(values) {
                *target = if value.is_null() { None } else {
                    Some(value.as_u64().ok_or("witness phase durations must be unsigned milliseconds or null")?)
                };
            }
        }
        Ok(Self { metadata: metadata.clone(),
            expected_settings: Some(crate::config::BenchmarkGraphicsSettings::from_json(get("expected_settings")?)?),
            requested_framebuffer,
            requested_camera: Some(requested_camera), requested_radius: unsigned(get("requested_radius")?)?,
            expected_declared_radius: unsigned(get("expected_declared_radius")?)?,
            expected_dimension: Some(nonempty("expected_dimension")?),
            expected_simulation_distance: Some(signed(get("expected_simulation_distance")?)?),
            expected_center: Some([signed(&center[0])?, signed(&center[1])?]),
            expected_domain_id: Some(nonempty("expected_domain_id")?), expected_domain,
            expected_render_domain, phase_durations_ms })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WitnessPolicy {
    pub max_sample_gap_ms: u64,
    pub boundary_tolerance_ms: u64,
    pub minimum_stable_ms: u64,
    pub position_tolerance: f64,
    pub angle_tolerance_degrees: f64,
    pub projection_tolerance: f64,
    pub required_effective_cap: Option<u32>,
}

impl Default for WitnessPolicy {
    fn default() -> Self {
        Self {
            max_sample_gap_ms: 250,
            boundary_tolerance_ms: 250,
            minimum_stable_ms: 100,
            position_tolerance: 0.001,
            angle_tolerance_degrees: 0.001,
            projection_tolerance: 0.0001,
            required_effective_cap: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Outcome { World, Menu, Skipped }

impl Outcome {
    fn name(self) -> &'static str {
        match self { Self::World => "world", Self::Menu => "menu", Self::Skipped => "skipped" }
    }
}

#[derive(Debug, Clone, Copy)]
struct Pending { phase: WitnessPhase, at_ms: u64, attempt: u64, witness: AttemptWitness }

#[derive(Debug)]
struct Row {
    pending: Pending,
    at_ms: u64,
    outcome: Outcome,
    camera: Option<CameraWitness>,
    draws: Option<[usize; 3]>,
    view: Option<ViewWitness>,
    failures: u32,
}

const MISSING_EXPECTATION: u32 = 1;
const FOREGROUND: u32 = 2;
const FRAMEBUFFER: u32 = 4;
const CAMERA: u32 = 8;
const SESSION: u32 = 16;
const VIEW_IDENTITY: u32 = 32;
const SETTLEMENT: u32 = 64;
const MISSING_WORLD: u32 = 128;
const SAMPLE_GAP: u32 = 256;
const PHASE_BOUNDARY: u32 = 512;
const OVERFLOW: u32 = 1024;
const INTERRUPTED: u32 = 2048;
const SETTINGS: u32 = 4096;
const INVALID_POLICY: u32 = 8192;

#[derive(Debug, Default)]
struct PhaseSummary {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    attempts: u64,
    worlds: u64,
    menus: u64,
    skipped: u64,
    paced: u64,
    samples: u64,
    first_sample_ms: Option<u64>,
    last_sample_ms: Option<u64>,
    max_gap_ms: u64,
    foreground_failures: u64,
    camera_failures: u64,
    nonfinite_cameras: u64,
    camera_min: Option<[f64; 9]>,
    camera_max: Option<[f64; 9]>,
    first_failure: Option<(u64, u64, u32)>,
    failures: u32,
    stable_start_ms: Option<u64>,
    stable_duration_ms: u64,
}

impl PhaseSummary {
    fn fail(&mut self, at_ms: u64, attempt: u64, failures: u32) {
        self.failures |= failures;
        if failures != 0 {
            self.stable_start_ms = None;
            self.stable_duration_ms = 0;
        }
        if failures != 0 && self.first_failure.is_none() {
            self.first_failure = Some((at_ms, attempt, failures));
        }
    }
}

#[derive(Debug)]
pub(crate) struct BenchmarkWitness {
    header: WitnessHeader,
    policy: WitnessPolicy,
    origin: Instant,
    phase: Option<WitnessPhase>,
    summaries: [PhaseSummary; PHASE_COUNT],
    rows: Vec<Row>,
    next_sample_ms: u64,
    pending: Option<Pending>,
    attempts: u64,
    dropped_rows: u64,
}

impl BenchmarkWitness {
    pub(crate) fn header(&self) -> &WitnessHeader { &self.header }

    pub(crate) fn new(header: WitnessHeader, policy: WitnessPolicy, now: Instant) -> Self {
        Self { header, policy, origin: now, phase: None,
            summaries: std::array::from_fn(|_| PhaseSummary::default()),
            rows: Vec::with_capacity(MAX_ROWS), next_sample_ms: 0, pending: None,
            attempts: 0, dropped_rows: 0 }
    }

    fn elapsed_ms(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.origin).as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn begin_attempt(&mut self, phase: WitnessPhase, now: Instant, witness: AttemptWitness) {
        self.flush_skipped();
        let at_ms = self.elapsed_ms(now);
        if self.phase != Some(phase) {
            if let Some(previous) = self.phase.filter(|p| *p != WitnessPhase::Complete) {
                self.summaries[previous as usize].end_ms = Some(at_ms);
            }
            if phase != WitnessPhase::Complete {
                let summary = &mut self.summaries[phase as usize];
                if summary.start_ms.is_some() {
                    summary.fail(at_ms, self.attempts, PHASE_BOUNDARY);
                } else {
                    summary.start_ms = Some(at_ms);
                }
            }
            self.phase = Some(phase);
        }
        if phase == WitnessPhase::Complete { return; }
        self.attempts += 1;
        let failure = self.attempt_failures(witness);
        let summary = &mut self.summaries[phase as usize];
        summary.attempts += 1;
        if failure & FOREGROUND != 0 { summary.foreground_failures += 1; }
        summary.fail(at_ms, self.attempts, failure);
        self.pending = Some(Pending { phase, at_ms, attempt: self.attempts, witness });
    }

    pub(crate) fn sample_due(&self, now: Instant) -> bool {
        self.pending.is_some() && self.elapsed_ms(now) >= self.next_sample_ms
            && self.rows.len() < MAX_ROWS
    }

    pub(crate) fn world_presented(
        &mut self, now: Instant, camera: CameraWitness, draws: [usize; 3], view: Option<ViewWitness>,
    ) {
        let Some(pending) = self.pending.take() else { return };
        let at_ms = self.elapsed_ms(now);
        let camera_failures = self.camera_failures(camera, pending.witness.framebuffer,
            pending.phase == WitnessPhase::Stationary);
        let summary = &mut self.summaries[pending.phase as usize];
        summary.worlds += 1;
        if camera_failures != 0 { summary.camera_failures += 1; }
        if !camera.finite() { summary.nonfinite_cameras += 1; }
        if camera.finite() {
            let values = camera.scalars();
            let min = summary.camera_min.get_or_insert(values);
            let max = summary.camera_max.get_or_insert(values);
            for index in 0..values.len() {
                min[index] = min[index].min(values[index]);
                max[index] = max[index].max(values[index]);
            }
        }
        summary.fail(at_ms, pending.attempt, camera_failures);
        self.record(pending, at_ms, Outcome::World, Some(camera), Some(draws), view);
    }

    pub(crate) fn menu_presented(&mut self, now: Instant) {
        let Some(pending) = self.pending.take() else { return };
        let at_ms = self.elapsed_ms(now);
        let summary = &mut self.summaries[pending.phase as usize];
        summary.menus += 1;
        if pending.phase == WitnessPhase::Stationary {
            summary.fail(at_ms, pending.attempt, MISSING_WORLD);
        }
        self.record(pending, at_ms, Outcome::Menu, None, None, None);
    }

    pub(crate) fn paced(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.summaries[pending.phase as usize].paced += 1;
        }
    }

    fn flush_skipped(&mut self) {
        let Some(pending) = self.pending.take() else { return };
        let summary = &mut self.summaries[pending.phase as usize];
        summary.skipped += 1;
        if pending.phase == WitnessPhase::Stationary {
            summary.fail(pending.at_ms, pending.attempt, MISSING_WORLD);
        }
        self.record(pending, pending.at_ms, Outcome::Skipped, None, None, None);
    }

    fn attempt_failures(&self, witness: AttemptWitness) -> u32 {
        let foreground = witness.foreground;
        let mut failures = 0;
        if !foreground.focused || !foreground.visible || !foreground.idle_seconds.is_finite()
            || foreground.effective_cap != self.policy.required_effective_cap {
            failures |= FOREGROUND;
        }
        if witness.framebuffer != self.header.requested_framebuffer
            || witness.framebuffer.contains(&0) { failures |= FRAMEBUFFER; }
        if !witness.session_connected || witness.loading || !witness.input_ready
            || !witness.player_feet.into_iter().all(f64::is_finite)
            || !witness.player_yaw_degrees.is_finite() || !witness.player_pitch_degrees.is_finite() {
            failures |= SESSION;
        }
        failures |= match witness.settings_match {
            Some(true) => 0,
            Some(false) => SETTINGS,
            None => MISSING_EXPECTATION,
        };
        failures
    }

    fn camera_failures(&self, actual: CameraWitness, framebuffer: [u32; 2], stationary: bool) -> u32 {
        if !actual.finite() || actual.near <= 0.0 || actual.far <= actual.near
            || actual.fov_y_degrees <= 0.0 || actual.fov_y_degrees >= 180.0
            || framebuffer.contains(&0) { return CAMERA; }
        let aspect = f64::from(framebuffer[0]) / f64::from(framebuffer[1]);
        let mut failures = if (actual.aspect - aspect).abs() > self.policy.projection_tolerance {
            CAMERA
        } else { 0 };
        if !stationary { return failures; }
        let Some(expected) = self.header.requested_camera.filter(|value| value.finite()) else {
            return failures | MISSING_EXPECTATION;
        };
        if actual.eye.into_iter().zip(expected.eye).any(|(a, b)| (a - b).abs() > self.policy.position_tolerance)
            || wrap_degrees(actual.yaw_degrees - expected.yaw_degrees).abs() > self.policy.angle_tolerance_degrees
            || (actual.pitch_degrees - expected.pitch_degrees).abs() > self.policy.angle_tolerance_degrees
            || [actual.fov_y_degrees - expected.fov_y_degrees, actual.aspect - expected.aspect]
                .into_iter().any(|value| value.abs() > self.policy.projection_tolerance) {
            failures |= CAMERA;
        }
        failures
    }

    fn view_failures(&self, view: Option<&ViewWitness>) -> u32 {
        let header = &self.header;
        if header.expected_dimension.is_none() || header.expected_center.is_none()
            || header.expected_simulation_distance.is_none() || header.expected_domain_id.is_none()
            || header.expected_domain.is_empty() || header.expected_domain.len() > 4_096
            || header.expected_render_domain.is_empty() {
            return MISSING_EXPECTATION;
        }
        let Some(view) = view else { return VIEW_IDENTITY | SETTLEMENT };
        let mut failures = 0;
        if view.dimension != header.expected_dimension || view.center != header.expected_center
            || view.center_source.is_none() || view.simulation_distance != header.expected_simulation_distance
            || view.requested_radius != header.requested_radius
            || view.declared_radius != Some(header.expected_declared_radius)
            || view.domain_id != header.expected_domain_id { failures |= VIEW_IDENTITY; }
        if !view.resident_domain.is_some_and(|(resident, expected)|
            expected == header.expected_domain.len() && resident == expected)
            || !view.render_domain.is_some_and(|counts| counts.expected == header.expected_render_domain.len()
            && counts.resident == counts.expected && counts.settled == counts.expected) {
            failures |= SETTLEMENT;
        }
        failures
    }

    fn record(&mut self, pending: Pending, at_ms: u64, outcome: Outcome,
        camera: Option<CameraWitness>, draws: Option<[usize; 3]>, view: Option<ViewWitness>) {
        if at_ms < self.next_sample_ms { return; }
        self.next_sample_ms = at_ms.saturating_add(CADENCE_MS);
        let mut failures = self.attempt_failures(pending.witness);
        if pending.phase == WitnessPhase::Stationary {
            failures |= match outcome {
                Outcome::World => self.view_failures(view.as_ref())
                    | camera.map_or(CAMERA, |camera| self.camera_failures(camera, pending.witness.framebuffer, true)),
                Outcome::Menu | Outcome::Skipped => MISSING_WORLD,
            };
        }
        let summary = &mut self.summaries[pending.phase as usize];
        let gap = summary.last_sample_ms.map(|last| at_ms.saturating_sub(last));
        if let Some(gap) = gap {
            summary.max_gap_ms = summary.max_gap_ms.max(gap);
            if gap > self.policy.max_sample_gap_ms { failures |= SAMPLE_GAP; }
        }
        if self.rows.len() == MAX_ROWS {
            self.dropped_rows += 1;
            summary.stable_start_ms = None;
            summary.fail(at_ms, pending.attempt, OVERFLOW);
            return;
        }
        summary.samples += 1;
        summary.first_sample_ms.get_or_insert(at_ms);
        summary.last_sample_ms = Some(at_ms);
        if failures == 0 {
            let start = *summary.stable_start_ms.get_or_insert(at_ms);
            summary.stable_duration_ms = at_ms.saturating_sub(start);
        } else {
            summary.stable_start_ms = None;
            summary.stable_duration_ms = 0;
        }
        summary.fail(at_ms, pending.attempt, failures);
        self.rows.push(Row { pending, at_ms, outcome, camera, draws, view, failures });
    }

    pub(crate) fn finish(mut self, now: Instant, interrupted: bool) -> String {
        self.flush_skipped();
        let at_ms = self.elapsed_ms(now);
        let mut failures = if interrupted { INTERRUPTED } else { 0 };
        if self.dropped_rows != 0 { failures |= OVERFLOW; }
        if self.header.expected_settings.is_none() { failures |= MISSING_EXPECTATION; }
        if self.policy.max_sample_gap_ms < CADENCE_MS || self.policy.boundary_tolerance_ms < CADENCE_MS
            || [self.policy.position_tolerance, self.policy.angle_tolerance_degrees,
                self.policy.projection_tolerance].into_iter().any(|v| !v.is_finite() || v < 0.0) {
            failures |= INVALID_POLICY;
        }
        if !self.header.phase_durations_ms[WitnessPhase::Stationary as usize].is_some_and(|v| v > 0) {
            failures |= MISSING_EXPECTATION;
        }
        for (index, summary) in self.summaries.iter_mut().enumerate() {
            let Some(expected_duration) = self.header.phase_durations_ms[index].filter(|v| *v > 0) else { continue };
            if let (Some(start), Some(end), Some(first), Some(last)) =
                (summary.start_ms, summary.end_ms, summary.first_sample_ms, summary.last_sample_ms) {
                if end.saturating_sub(start).abs_diff(expected_duration) > self.policy.boundary_tolerance_ms
                    || first.saturating_sub(start) > self.policy.boundary_tolerance_ms
                    || end.saturating_sub(last) > self.policy.boundary_tolerance_ms {
                    summary.fail(at_ms, self.attempts, PHASE_BOUNDARY);
                }
            } else { summary.fail(at_ms, self.attempts, PHASE_BOUNDARY); }
            if summary.max_gap_ms > self.policy.max_sample_gap_ms { summary.fail(at_ms, self.attempts, SAMPLE_GAP); }
            if index == WitnessPhase::Stationary as usize {
                if summary.worlds == 0 { summary.fail(at_ms, self.attempts, MISSING_WORLD); }
                if summary.stable_duration_ms < self.policy.minimum_stable_ms { summary.fail(at_ms, self.attempts, SETTLEMENT); }
                failures |= summary.failures;
            } else {
                failures |= summary.failures & (PHASE_BOUNDARY | SAMPLE_GAP | OVERFLOW);
            }
        }
        let phases: Vec<_> = [WitnessPhase::WaitingForJoin, WitnessPhase::Warmup,
            WitnessPhase::Mutation, WitnessPhase::Stationary, WitnessPhase::Moving]
            .into_iter().zip(&self.summaries).map(|(phase, summary)| phase_json(phase, summary)).collect();
        let rows: Vec<_> = self.rows.iter().map(row_json).collect();
        json!({"schema": 1, "boundary": "production surface submission",
            "settlement_evidence": "current-view-settlement-at-sample",
            "continuous_settlement_proven": false,
            "camera_equality_scope": ["eye", "wrapped_yaw", "pitch", "vertical_fov", "aspect"],
            "clip_plane_evidence": "finite positive near; far greater than near; equality not required",
            "foreground_evidence": "pacer state; observed-event flags retained for external confirmation",
            "metadata_evidence": "caller declarations; world, asset and protocol identities require external confirmation",
            "status": if interrupted { "interrupted" } else { "complete" },
            "accepted": failures == 0, "failures": failure_names(failures),
            "metadata": self.header.metadata,
            "expected_settings": self.header.expected_settings.map(settings_json),
            "requested": {"framebuffer": self.header.requested_framebuffer,
                "camera": self.header.requested_camera.map(CameraWitness::json),
                "radius": self.header.requested_radius,
                "declared_radius": self.header.expected_declared_radius, "dimension": self.header.expected_dimension,
                "simulation_distance": self.header.expected_simulation_distance,
                "center": self.header.expected_center, "domain_id": self.header.expected_domain_id,
                "domain_columns": self.header.expected_domain.len(), "domain": self.header.expected_domain,
                "render_domain": self.header.expected_render_domain,
                "phase_durations_ms": self.header.phase_durations_ms},
            "policy": {"cadence_ms": CADENCE_MS, "max_rows": MAX_ROWS,
                "max_sample_gap_ms": self.policy.max_sample_gap_ms,
                "boundary_tolerance_ms": self.policy.boundary_tolerance_ms,
                "minimum_stable_ms": self.policy.minimum_stable_ms,
                "position_tolerance": self.policy.position_tolerance,
                "angle_tolerance_degrees": self.policy.angle_tolerance_degrees,
                "projection_tolerance": self.policy.projection_tolerance,
                "required_effective_cap": self.policy.required_effective_cap},
            "elapsed_ms": at_ms, "dropped_rows": self.dropped_rows, "phases": phases, "rows": rows}).to_string()
    }
}

fn wrap_degrees(value: f64) -> f64 { (value + 180.0).rem_euclid(360.0) - 180.0 }

fn settings_json(settings: crate::config::BenchmarkGraphicsSettings) -> Value {
    let mut options = crate::config::Options::default();
    settings.apply(&mut options);
    json!({"framerate_limit": options.framerate_limit, "enable_vsync": options.enable_vsync,
        "inactivity_fps_limit": crate::config::inactivity_fps_limit_name(options.inactivity_fps_limit),
        "graphics_preset": crate::config::graphics_preset_name(options.graphics_preset),
        "cloud_status": crate::config::cloud_status_name(options.cloud_status),
        "cutout_leaves": options.cutout_leaves, "entity_shadows": options.entity_shadows,
        "particles": crate::config::particle_level_name(options.particles), "fov": options.fov,
        "render_distance": options.render_distance, "biome_blend_radius": options.biome_blend_radius})
}

fn failure_names(bits: u32) -> Vec<&'static str> {
    [(MISSING_EXPECTATION, "missing-independent-expectation"), (FOREGROUND, "foreground-or-cap"),
        (FRAMEBUFFER, "framebuffer-mismatch"), (CAMERA, "camera-mismatch-or-nonfinite"),
        (SESSION, "session-loading-or-input"), (VIEW_IDENTITY, "view-identity-mismatch"),
        (SETTLEMENT, "target-domain-not-settled"), (MISSING_WORLD, "missing-world-handoff"),
        (SAMPLE_GAP, "sample-gap"), (PHASE_BOUNDARY, "missing-or-late-phase-boundary"),
        (OVERFLOW, "row-overflow"), (INTERRUPTED, "interrupted"), (SETTINGS, "settings-mismatch"),
        (INVALID_POLICY, "invalid-policy")]
        .into_iter().filter_map(|(bit, name)| (bits & bit != 0).then_some(name)).collect()
}

fn phase_json(phase: WitnessPhase, summary: &PhaseSummary) -> Value {
    json!({"phase": phase.name(), "start_ms": summary.start_ms, "end_ms": summary.end_ms,
        "start_witnessed": summary.start_ms.is_some(), "end_witnessed": summary.end_ms.is_some(),
        "attempts": summary.attempts, "world_handoffs": summary.worlds,
        "menu_handoffs": summary.menus, "skipped": summary.skipped,
        "paced_attempts": summary.paced, "samples": summary.samples,
        "first_sample_ms": summary.first_sample_ms, "last_sample_ms": summary.last_sample_ms,
        "max_sample_gap_ms": summary.max_gap_ms, "foreground_failures": summary.foreground_failures,
        "camera_failures": summary.camera_failures, "nonfinite_cameras": summary.nonfinite_cameras,
        "camera_extrema_order": ["eye_x", "eye_y", "eye_z", "wrapped_yaw_degrees", "pitch_degrees",
            "fov_y_degrees", "aspect", "near", "far"],
        "camera_min": summary.camera_min, "camera_max": summary.camera_max,
        "first_failure": summary.first_failure.map(|(at, attempt, bits)| json!({"at_ms": at,
            "attempt": attempt, "failures": failure_names(bits)})),
        "failures": failure_names(summary.failures), "stable_duration_ms": summary.stable_duration_ms})
}

fn row_json(row: &Row) -> Value {
    let witness = row.pending.witness;
    let foreground = witness.foreground;
    json!({"phase": row.pending.phase.name(), "at_ms": row.at_ms,
        "attempt_at_ms": row.pending.at_ms, "attempt": row.pending.attempt,
        "outcome": row.outcome.name(), "framebuffer": witness.framebuffer,
        "foreground": {"focused": foreground.focused, "visible": foreground.visible,
            "focus_observed": foreground.focus_observed, "visibility_observed": foreground.visibility_observed,
            "idle_seconds": foreground.idle_seconds, "effective_cap": foreground.effective_cap},
        "session_connected": witness.session_connected, "loading": witness.loading,
        "input_ready": witness.input_ready, "settings_match": witness.settings_match, "player_feet": witness.player_feet,
        "player_yaw_degrees": witness.player_yaw_degrees, "player_pitch_degrees": witness.player_pitch_degrees,
        "camera": row.camera.map(CameraWitness::json), "draws_opaque_water_translucent": row.draws,
        "view": row.view.as_ref().map(|view| json!({"dimension": view.dimension,
            "simulation_distance": view.simulation_distance, "requested_radius": view.requested_radius,
            "declared_radius": view.declared_radius, "center": view.center,
            "center_source": view.center_source.map(CenterSource::name),
            "square_diagnostic_only": view.square_diagnostic.map(CoverageWitness::json),
            "domain_id": view.domain_id,
            "resident_domain": view.resident_domain.map(|(resident, expected)| json!({"resident":resident,"expected":expected})),
            "render_domain": view.render_domain.map(CoverageWitness::json),
            "mesh_waiting_columns": view.mesh_backlog, "pending_meshes": view.pending_meshes})),
        "failures": failure_names(row.failures)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn camera() -> CameraWitness {
        CameraWitness { eye: [0.0, 141.619999886, 0.0], yaw_degrees: 0.0,
            pitch_degrees: 10.0, fov_y_degrees: 70.0, aspect: 16.0 / 9.0, near: 0.05, far: 256.0 }
    }

    fn header() -> WitnessHeader {
        let settings = json!({"framerate_limit":260,"enable_vsync":false,"inactivity_fps_limit":"minimized",
            "graphics_preset":"custom","cloud_status":"off","cutout_leaves":false,
            "entity_shadows":false,"particles":"minimal","fov":70,"render_distance":8,"biome_blend_radius":0});
        WitnessHeader { metadata: json!({"fixture":"independent-control"}),
            expected_settings: Some(crate::config::BenchmarkGraphicsSettings::from_json(&settings).unwrap()),
            requested_framebuffer: [1280, 720], requested_camera: Some(camera()), requested_radius: 1,
            expected_declared_radius: 1,
            expected_dimension: Some("minecraft:overworld".into()), expected_simulation_distance: Some(8),
            expected_center: Some([0, 0]), expected_domain_id: Some("literal-nine-columns".into()),
            expected_domain: vec![[-1,-1],[0,-1],[1,-1],[-1,0],[0,0],[1,0],[-1,1],[0,1],[1,1]],
            expected_render_domain: vec![[0,0]],
            phase_durations_ms: [None,None,None,Some(300),None] }
    }

    fn attempt() -> AttemptWitness {
        AttemptWitness { framebuffer:[1280,720], foreground:ForegroundWitness {
            focused:true,visible:true,focus_observed:true,visibility_observed:true,idle_seconds:0.0,effective_cap:None },
            session_connected:true,loading:false,input_ready:true,player_feet:[0.0,140.0,0.0],
            player_yaw_degrees:0.0,player_pitch_degrees:10.0,settings_match:Some(true) }
    }

    fn view() -> ViewWitness {
        ViewWitness { dimension:Some("minecraft:overworld".into()),simulation_distance:Some(8),
            requested_radius:1,declared_radius:Some(1),center:Some([0,0]),center_source:Some(CenterSource::Server),
            square_diagnostic:Some(CoverageWitness {resident:9,settled:9,expected:9}),
            domain_id:Some("literal-nine-columns".into()),resident_domain:Some((9,9)),
            render_domain:Some(CoverageWitness {resident:1,settled:1,expected:1}),
            mesh_backlog:0,pending_meshes:0 }
    }

    fn trial(change: impl Fn(&mut AttemptWitness, &mut CameraWitness, &mut ViewWitness)) -> Value {
        let start = Instant::now();
        let mut collector = BenchmarkWitness::new(header(), WitnessPolicy::default(), start);
        for ms in [0,100,200] {
            let (mut attempt,mut camera,mut view) = (attempt(),camera(),view());
            if ms == 100 { change(&mut attempt,&mut camera,&mut view); }
            let now = start + Duration::from_millis(ms);
            collector.begin_attempt(WitnessPhase::Stationary, now, attempt);
            collector.world_presented(now, camera, [4,0,0], Some(view));
        }
        let stop = start + Duration::from_millis(300);
        collector.begin_attempt(WitnessPhase::Complete, stop, attempt());
        serde_json::from_str(&collector.finish(stop, false)).unwrap()
    }

    #[test]
    fn cadence_phase_edges_stalls_and_capacity_are_bounded() {
        let start = Instant::now();
        let mut collector = BenchmarkWitness::new(header(), WitnessPolicy::default(), start);
        for ms in [0,99,100,150,200,201,550] {
            let now = start + Duration::from_millis(ms);
            let phase = if ms < 150 { WitnessPhase::Warmup } else { WitnessPhase::Stationary };
            collector.begin_attempt(phase, now, attempt());
            let due = collector.sample_due(now);
            collector.world_presented(now, camera(), [4,0,0], due.then(view));
        }
        assert_eq!(collector.rows.iter().map(|row| row.at_ms).collect::<Vec<_>>(), [0,100,200,550]);
        assert_eq!(collector.summaries[WitnessPhase::Stationary as usize].max_gap_ms, 350);
        assert_eq!(collector.summaries[WitnessPhase::Warmup as usize].end_ms, Some(150));
        let mut collector = BenchmarkWitness::new(header(), WitnessPolicy::default(), start);
        for index in 0..=MAX_ROWS {
            let now = start + Duration::from_millis(index as u64 * 100);
            collector.begin_attempt(WitnessPhase::Warmup, now, attempt());
            collector.world_presented(now, camera(), [4,0,0], None);
        }
        assert_eq!((collector.rows.len(),collector.dropped_rows), (2048,1));
        assert!(!collector.sample_due(start + Duration::from_secs(300)));
    }

    #[test]
    fn independent_fixture_accepts_and_planted_mismatches_are_rejected() {
        let positive = trial(|_,_,_| {});
        assert_eq!(positive["accepted"], true);
        assert_eq!(positive["rows"][0]["camera"]["eye"][1], 141.619999886);
        assert_ne!(positive["rows"][0]["camera"]["eye"], positive["rows"][0]["player_feet"]);
        for report in [
            trial(|_,camera,_| camera.yaw_degrees += 7.0),
            trial(|_,camera,_| camera.eye[0] += 0.25),
            trial(|attempt,camera,_| {attempt.framebuffer[1]=800;camera.aspect=1.6;}),
            trial(|attempt,_,_| attempt.foreground.focused=false),
            trial(|attempt,_,_| attempt.settings_match=Some(false)),
            trial(|_,_,view| view.declared_radius=Some(8)),
            trial(|_,_,view| view.center=Some([-1,0])),
            trial(|_,_,view| view.resident_domain.as_mut().unwrap().0=8),
            trial(|_,_,view| view.render_domain.as_mut().unwrap().settled=0),
            trial(|_,_,view| view.render_domain.as_mut().unwrap().expected=289),
            trial(|_,camera,_| camera.eye[1]=f64::NAN),
        ] { assert_eq!(report["accepted"], false, "{report}"); }
        let wrapped = trial(|_,camera,_| camera.yaw_degrees=360.0);
        assert_eq!(wrapped["accepted"], true);
        let diagnostic_only = trial(|_,_,view| view.square_diagnostic.as_mut().unwrap().settled=0);
        assert_eq!(diagnostic_only["accepted"], true);
        let start = Instant::now();
        let mut collector = BenchmarkWitness::new(header(), WitnessPolicy::default(), start);
        for ms in [0,50,100,200] {
            let now = start + Duration::from_millis(ms);
            let mut actual = camera();
            if ms == 50 { actual.yaw_degrees=7.0; }
            collector.begin_attempt(WitnessPhase::Stationary, now, attempt());
            let due = collector.sample_due(now);
            collector.world_presented(now, actual, [4,0,0], due.then(view));
        }
        let stop = start + Duration::from_millis(300);
        collector.begin_attempt(WitnessPhase::Complete, stop, attempt());
        let report: Value = serde_json::from_str(&collector.finish(stop,false)).unwrap();
        assert_eq!(report["accepted"], false);
        assert_eq!(report["rows"].as_array().unwrap().len(), 3);
        assert_eq!(report["phases"][3]["first_failure"]["at_ms"], 50);
    }
}
