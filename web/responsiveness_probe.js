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
    if (!this.latest.has(category) && this.latest.size >= 40) return;
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

const JOIN_PHASES = new Set([
  "world-create-started", "world-open-started", "first-terrain-presented", "gameplay-ready",
  "loading-overlay-ready", "full-view-presented", "full-view-quiescent",
]);
const JOIN_PROGRESS_FIELDS = [
  "elapsedMs", "loadedColumns", "expectedColumns", "settledColumns", "pendingMeshes",
  "pendingLightRemeshes", "presentedColumns", "pendingColumns", "pendingRemovals",
];

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
  window.addEventListener("pagehide", () => finish("pagehide"));
}
