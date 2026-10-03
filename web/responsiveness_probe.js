export class ResponsivenessProbe {
  constructor(send, now = () => performance.now()) {
    this.send = send;
    this.now = now;
    this.active = null;
    this.latest = new Map();
    this.terrainPresented = false;
    this.overlayReady = false;
    this.gameplayReady = false;
    this.joinSequence = 0;
    this.join = null;
    this.blockActions = [];
    this.droppedBlockActions = 0;
  }

  observe(data) {
    if (data.kind === "progress") {
      const phase = data.event?.phase;
      if (phase === "block-action-trace") {
        if (typeof data.event.message !== "string") return;
        if (this.blockActions.length === 32) {
          this.blockActions.shift();
          this.droppedBlockActions++;
        }
        this.blockActions.push({ atMs: this.now(), message: data.event.message });
        return true;
      }
      if (phase === "world-create-started" || phase === "world-open-started") {
        this.terrainPresented = false;
        this.overlayReady = false;
        this.gameplayReady = false;
        this.blockActions.length = 0;
        this.droppedBlockActions = 0;
        this.join = {
          sequence: ++this.joinSequence,
          startPhase: phase,
          startedAtMs: this.now(),
          runtime: null,
          milestones: [],
        };
      } else if (phase === "first-terrain-presented") {
        if (!this.join) return;
        this.terrainPresented = true;
      } else if (phase === "loading-overlay-ready") {
        if (!this.join) return;
        this.overlayReady = true;
      } else if (phase === "gameplay-ready") {
        if (!this.join) return;
        this.gameplayReady = true;
      }
      if (!this.join || !JOIN_PHASES.has(phase)) return;
      if (this.join.milestones.some(milestone => milestone.phase === phase)) return;
      const event = data.event;
      const milestone = { phase, atMs: this.now() };
      for (const field of JOIN_PROGRESS_FIELDS) {
        if (Number.isFinite(event[field])) milestone[field] = event[field];
      }
      this.join.milestones.push(milestone);
      return true;
    }
    if (data.kind !== "diagnostic" || typeof data.message !== "string") return;
    const runtime = /^server startup: phase=preparing-world executor=(serial|threaded) workers=([1-9]\d*)$/.exec(data.message);
    if (runtime) {
      if (this.join) this.join.runtime = { executor: runtime[1], workers: Number(runtime[2]) };
      return true;
    }
    let category = data.message.split(":", 1)[0];
    if (!/^(connection|server health|transport|wasm mesh|view|worldgen timing)/.test(category)) return;
    const timing = /^worldgen timing: phase=([a-z-]+) calls=(\d+) items=(\d+) sum_ms=([\d.]+) max_ms=([\d.]+)$/.exec(data.message);
    if (timing) category += `:${timing[1]}`;
    if (category === "wasm mesh passes") {
      const cause = meshPassCause(data.message);
      if (!cause) return;
      category += `:${cause}`;
    }
    if (this.active) recordDiagnostic(this.active.diagnosticSummary, data.message);
    if (!this.latest.has(category) && this.latest.size >= 64) return;
    if (timing) {
      if (this.active) {
        const phases = this.active.generationPhases;
        const total = phases.get(timing[1]) ?? {
          phase: timing[1], calls: 0, items: 0, elapsedMs: 0, maxMs: 0,
        };
        total.calls += Number(timing[2]);
        total.items += Number(timing[3]);
        total.elapsedMs += Number(timing[4]);
        total.maxMs = Math.max(total.maxMs, Number(timing[5]));
        phases.set(total.phase, total);
      }
    }
    const sample = { atMs: this.now(), message: data.message };
    this.latest.set(category, sample);
    if (this.active) {
      this.active.samplesSeen++;
      if (this.active.samples.length < 256) this.active.samples.push(sample);
    }
  }

  get playable() {
    return this.terrainPresented && (this.gameplayReady || this.overlayReady);
  }

  get joinReport() {
    return this.join ? {
      sequence: this.join.sequence,
      startPhase: this.join.startPhase,
      startedAtMs: this.join.startedAtMs,
      runtime: this.join.runtime ? { ...this.join.runtime } : null,
      milestones: this.join.milestones.map(milestone => ({ ...milestone })),
    } : null;
  }

  get blockActionReport() {
    return {
      rows: this.blockActions.map(row => ({ ...row })),
      droppedRows: this.droppedBlockActions,
    };
  }

  aimDown() {
    if (this.active) throw new Error("a responsiveness probe is already running");
    this.send({ type: "focus", focused: true });
    this.send({ type: "pointerLock", locked: true });
    this.send({ type: "mouseMotion", dx: 0, dy: 600 });
  }

  start(mode) {
    if (this.active) throw new Error("a responsiveness probe is already running");
    if (mode !== "walk" && mode !== "mine") throw new Error("unknown probe mode");
    this.active = {
      mode, startedMs: this.now(), endedMs: null, samplesSeen: 0,
      baseline: [...this.latest.values()], samples: [], generationPhases: new Map(),
      diagnosticSummary: {},
    };
    this.send({ type: "focus", focused: true });
    this.send({ type: "pointerLock", locked: true });
    if (mode === "walk") {
      for (const code of ["ControlLeft", "Space", "KeyW"]) this.key(code, true);
    } else {
      this.send({ type: "mouseButton", button: 0, pressed: true });
    }
  }

  key(code, pressed) {
    this.send({ type: "key", code, pressed, modifiers: 0 });
  }

  stop(reason = "completed") {
    if (!this.active) return null;
    for (const code of ["KeyW", "ControlLeft", "Space"]) this.key(code, false);
    this.send({ type: "mouseButton", button: 0, pressed: false });
    const report = this.active;
    this.active = null;
    report.endedMs = this.now();
    report.durationMs = report.endedMs - report.startedMs;
    report.reason = reason;
    report.final = [...this.latest.values()];
    report.join = this.joinReport;
    report.blockActions = this.blockActionReport;
    report.generationPhases = [...report.generationPhases.values()];
    report.samplesTruncated = report.samplesSeen > report.samples.length;
    return report;
  }
}

const DIAGNOSTIC_FIELDS = {
  "view presentation": { group: "view", fields: {
    missing_resident: ["missingResident"], resident_unpresented: ["residentUnpresented"],
    submitted_meshes: ["pendingMeshes"], pending_light_remeshes: ["pendingLightRemeshes"],
    pending_columns: ["pendingColumns"], pending_removals: ["pendingRemovals"],
  } },
  "server health": { group: "server", fields: {
    mspt_ms: ["msptMs"], overruns: ["overruns"], callback_gap_ms: ["callbackGapMs"],
    wake_p95_ms: ["wakeP95Ms"], wake_max_ms: ["wakeMaxMs"], deadline_max_ms: ["deadlineMaxMs"],
  } },
  "wasm mesh drain and upload profile": { group: "mesh", fields: {
    "frame_gap_p95/max_ms": ["frameGapP95Ms", "frameGapMaxMs"],
    "drain_p95/max_ms": ["drainP95Ms", "drainMaxMs"],
    "upload_p95/max_ms": ["uploadP95Ms", "uploadMaxMs"],
    "backlog_ready/waiting/forced/sections_max": ["readyColumns", "waitingColumns", "forcedColumns", "pendingSections"],
  } },
  "wasm mesh queue": { group: "meshQueue", fields: {
    queued_keys: ["queuedKeys"], oldest_wait_ms: ["oldestWaitMs"],
  } },
  "wasm mesh light sources": { group: "meshLightSources", fields: {
    "app_local_totals_blocks/jobs/visited/changed/unchanged_skips/equivalent_skips": [
      "appLocalBlocks", "appLocalJobs", "appLocalCellsVisited", "appLocalCellsChanged",
      "appLocalUnchangedSkips", "appLocalEquivalentSkips",
    ],
    "session_patch_totals_calls/queued/boundary_skips/absorbed": [
      "sessionPatchCalls", "sessionPatchQueued", "sessionPatchBoundarySkips", "sessionPatchAbsorbed",
    ],
  } },
  "wasm mesh passes": { group: "meshPasses", fields: {
    "totals_built/applied/unchanged/failed": ["sessionBuilt", "sessionApplied", "sessionUnchanged", "sessionFailed"],
    capture_calls: ["sessionCaptureCalls"],
    "total_capture/model/fluid/visibility/packing/hash_ms": [
      "sessionCaptureTotalMs", "sessionModelTotalMs", "sessionFluidTotalMs",
      "sessionVisibilityTotalMs", "sessionPackingTotalMs", "sessionHashTotalMs",
    ],
    "max_capture/model/fluid/visibility/packing/hash_ms": [
      "sessionCaptureMaxMs", "sessionModelMaxMs", "sessionFluidMaxMs",
      "sessionVisibilityMaxMs", "sessionPackingMaxMs", "sessionHashMaxMs",
    ],
  } },
};

function meshPassCause(message) {
  return /^wasm mesh passes: cause=(Column|Section|Light|Explicit)(?:\s|$)/.exec(message)?.[1];
}

function recordDiagnostic(summary, message) {
  const separator = message.indexOf(":");
  const category = message.slice(0, separator);
  if (!Object.hasOwn(DIAGNOSTIC_FIELDS, category)) return;
  const { fields } = DIAGNOSTIC_FIELDS[category];
  let group = DIAGNOSTIC_FIELDS[category].group;
  if (category === "wasm mesh passes") {
    const cause = meshPassCause(message);
    if (!cause) return;
    group += cause;
  }
  let recorded = false;
  for (const field of message.slice(separator + 1).trim().split(/\s+/)) {
    const [key, encoded] = field.split("=");
    if (!Object.hasOwn(fields, key) || encoded === undefined) continue;
    const values = encoded.split("/");
    if (values.length !== fields[key].length) continue;
    fields[key].forEach((name, index) => {
      const value = values[index].replace(/^Some\((.*)\)$/, "$1");
      if (!/^\d+(?:\.\d+)?(?:[eE][+-]?\d+)?$/.test(value)) return;
      const number = Number(value);
      if (!Number.isFinite(number)) return;
      const totals = summary[group] ??= { samples: 0, metrics: {} };
      const metric = totals.metrics[name] ??= { samples: 0, maximum: number };
      metric.samples++;
      metric.maximum = Math.max(metric.maximum, number);
      recorded = true;
    });
  }
  if (recorded) summary[group].samples++;
}

const JOIN_PHASES = new Set([
  "world-create-started", "world-open-started", "first-terrain-presented", "gameplay-ready",
  "loading-overlay-ready", "full-view-presented", "full-view-quiescent",
]);
const JOIN_PROGRESS_FIELDS = [
  "elapsedMs", "loadedColumns", "expectedColumns", "settledColumns", "pendingMeshes",
  "pendingLightRemeshes", "presentedColumns", "pendingColumns", "pendingRemovals",
];

export class PresentationProbe {
  constructor(send) {
    this.send = send;
    this.phase = "idle";
    this.report = null;
    this.error = null;
  }

  get active() {
    return this.phase === "requested" || this.phase === "recording";
  }

  start() {
    if (this.active) return false;
    this.phase = "requested";
    this.report = null;
    this.error = null;
    this.send({ type: "startPresentationCapture" });
    return true;
  }

  observe(event) {
    if (!this.active) return false;
    if (event?.phase === "presentation-capture-started" && this.phase === "requested") {
      this.phase = "recording";
    } else if (event?.phase === "presentation-capture-complete") {
      if (typeof event.message !== "string" || event.message.length > 704512) {
        this.phase = "error";
        this.error = "invalid or oversized presentation report";
      } else {
        this.phase = "complete";
        this.report = event.message;
      }
    } else if (event?.phase === "presentation-capture-error") {
      this.phase = "error";
      this.error = event.message;
    } else {
      return false;
    }
    return true;
  }
}

export function install(worker, canvas) {
  if (new URLSearchParams(location.search).get("probe") !== "1") return;
  const panel = document.createElement("aside");
  panel.style.cssText = "position:fixed;right:8px;top:8px;z-index:10;padding:8px;background:#111d;color:#fff;font:12px monospace;max-width:320px";
  const status = document.createElement("div");
  status.textContent = "Probe: join a world first";
  panel.append(status);
  const reportNode = document.createElement("script");
  reportNode.id = "lodestone-responsiveness-report";
  reportNode.type = "application/json";
  panel.append(reportNode);
  document.body.append(panel);
  const probe = new ResponsivenessProbe(input => worker.postMessage({ kind: "input", input }));
  const presentation = new PresentationProbe(probe.send);
  const framesNode = document.createElement("script");
  framesNode.id = "lodestone-presentation-report";
  framesNode.type = "application/json";
  panel.append(framesNode);
  const framesStatus = document.createElement("div");
  const frames = document.createElement("button");
  frames.textContent = "Capture frames 10s";
  frames.onpointerdown = event => event.preventDefault();
  frames.onclick = () => {
    if (!presentation.start()) return;
    frames.disabled = true;
    framesNode.textContent = "";
    framesStatus.textContent = "Frames: waiting for capture acknowledgment";
  };
  panel.append(frames, framesStatus);
  let framesTimer = null;
  let timer = null;
  let playable = false;
  let displayedReport = null;
  let displayedJoinSequence = null;
  const buttons = [];
  const finish = reason => {
    clearTimeout(timer);
    timer = null;
    const report = probe.stop(reason);
    if (report) {
      displayedReport = report;
      displayedJoinSequence = report.join?.sequence ?? null;
      reportNode.textContent = JSON.stringify(report);
      console.info("lodestone responsiveness probe", report);
      status.textContent = `${report.mode}: ${(report.durationMs / 1000).toFixed(2)}s, ${report.samplesSeen} samples (${reason})`;
    }
    for (const button of buttons) button.disabled = !playable;
  };
  const publishJoinReport = () => {
    const join = probe.joinReport;
    if (displayedReport && displayedJoinSequence === (join?.sequence ?? null)) {
      displayedReport.join = join;
      displayedReport.blockActions = probe.blockActionReport;
      reportNode.textContent = JSON.stringify(displayedReport);
      return;
    }
    displayedReport = null;
    displayedJoinSequence = join?.sequence ?? null;
    reportNode.textContent = JSON.stringify({ join, blockActions: probe.blockActionReport });
  };
  let traceEnabled = new URLSearchParams(location.search).get("trace-block-actions") === "1";
  const trace = document.createElement("button");
  const updateTraceLabel = () => { trace.textContent = `Trace blocks: ${traceEnabled ? "on" : "off"}`; };
  updateTraceLabel();
  trace.onpointerdown = event => event.preventDefault();
  trace.onclick = () => {
    traceEnabled = !traceEnabled;
    probe.send({ type: "setBlockActionTrace", enabled: traceEnabled });
    updateTraceLabel();
  };
  panel.append(trace);
  for (const [label, mode, duration] of [
    ["Walk 20s", "walk", 20000], ["Mine 3s", "mine", 3000],
  ]) {
    const button = document.createElement("button");
    button.textContent = label;
    button.disabled = true;
    button.onpointerdown = event => event.preventDefault();
    button.onclick = () => {
      canvas.focus();
      probe.start(mode);
      status.textContent = `${mode}: running`;
      for (const control of buttons) control.disabled = true;
      timer = setTimeout(() => finish("completed"), duration);
    };
    panel.append(button);
    buttons.push(button);
  }
  const aim = document.createElement("button");
  aim.textContent = "Aim down";
  aim.disabled = true;
  aim.onpointerdown = event => event.preventDefault();
  aim.onclick = () => {
    canvas.focus();
    probe.aimDown();
    status.textContent = "Aim applied; check the block target before mining";
  };
  panel.append(aim);
  buttons.push(aim);
  const stop = document.createElement("button");
  stop.textContent = "Stop probe";
  stop.onpointerdown = event => event.preventDefault();
  stop.onclick = () => finish("stopped");
  panel.append(stop);
  const save = document.createElement("button");
  save.textContent = "Save metrics";
  save.onpointerdown = event => event.preventDefault();
  save.onclick = () => {
    const reports = {
      responsiveness: reportNode.textContent ? JSON.parse(reportNode.textContent) : null,
      presentation: framesNode.textContent ? JSON.parse(framesNode.textContent) : null,
    };
    const url = URL.createObjectURL(new Blob([JSON.stringify(reports)], { type: "application/json" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = "lodestone-metrics.json";
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  panel.append(save);
  worker.addEventListener("message", event => {
    const data = event.data;
    const phase = data.kind === "progress" ? data.event?.phase : undefined;
    const joining = phase === "world-create-started" || phase === "world-open-started";
    if (joining) {
      playable = false;
      finish("new-join");
    }
    const joinChanged = probe.observe(data);
    if (data.kind === "progress") {
      if (presentation.observe(data.event)) {
        if (presentation.phase === "recording") {
          clearTimeout(framesTimer);
          framesTimer = setTimeout(() => probe.send({ type: "stopPresentationCapture" }), 10000);
          framesStatus.textContent = "Frames: recording";
        } else {
          clearTimeout(framesTimer);
          framesTimer = null;
          frames.disabled = false;
          framesNode.textContent = presentation.report ?? "";
          framesStatus.textContent = presentation.error ? `Frames: ${presentation.error}` : "Frames: captured";
        }
      }
      if (joinChanged) publishJoinReport();
      if (probe.playable && !playable) {
        playable = true;
        for (const button of buttons) button.disabled = !!probe.active;
        status.textContent = "Probe ready; aim at a block before mining";
      } else if (joining) {
        status.textContent = "Joining world; responsiveness probes are unavailable";
      }
    } else if (data.kind === "error") {
      playable = false;
      finish("worker-error");
      status.textContent = `Worker error: ${data.message ?? "unknown error"}`;
    } else if (joinChanged) {
      publishJoinReport();
    }
  });
  window.addEventListener("pagehide", () => {
    clearTimeout(framesTimer);
    if (presentation.active) probe.send({ type: "stopPresentationCapture" });
    finish("pagehide");
  });
}
