export class ResponsivenessProbe {
  constructor(send, now = () => performance.now()) {
    this.send = send;
    this.now = now;
    this.active = null;
    this.latest = new Map();
    this.terrainPresented = false;
    this.overlayReady = false;
  }

  observe(data) {
    if (data.kind === "progress") {
      const phase = data.event?.phase;
      if (phase === "world-create-started" || phase === "world-open-started") {
        this.terrainPresented = false;
        this.overlayReady = false;
      } else if (phase === "first-terrain-presented") {
        this.terrainPresented = true;
      } else if (phase === "loading-overlay-ready") {
        this.overlayReady = true;
      }
      return;
    }
    if (data.kind !== "diagnostic" || typeof data.message !== "string") return;
    const category = data.message.split(":", 1)[0];
    if (!/^(connection|server health|transport|wasm mesh|view|worldgen timing)/.test(category)) return;
    const sample = { atMs: this.now(), message: data.message };
    this.latest.set(category, sample);
    if (this.active) {
      this.active.samplesSeen++;
      if (this.active.samples.length < 256) this.active.samples.push(sample);
    }
  }

  get playable() {
    return this.terrainPresented && this.overlayReady;
  }

  start(mode) {
    if (this.active) throw new Error("a responsiveness probe is already running");
    if (mode !== "walk" && mode !== "mine") throw new Error("unknown probe mode");
    this.active = {
      mode, startedMs: this.now(), endedMs: null, samplesSeen: 0,
      baseline: [...this.latest.values()], samples: [],
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
    this.send({ type: "pointerLock", locked: false });
    const report = this.active;
    this.active = null;
    report.endedMs = this.now();
    report.durationMs = report.endedMs - report.startedMs;
    report.reason = reason;
    report.final = [...this.latest.values()];
    report.samplesTruncated = report.samplesSeen > report.samples.length;
    return report;
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
  let timer = null;
  let playable = false;
  const buttons = [];
  const finish = reason => {
    clearTimeout(timer);
    timer = null;
    const report = probe.stop(reason);
    if (report) {
      reportNode.textContent = JSON.stringify(report);
      console.info("lodestone responsiveness probe", report);
      status.textContent = `${report.mode}: ${(report.durationMs / 1000).toFixed(2)}s, ${report.samplesSeen} samples (${reason})`;
    }
    for (const button of buttons) button.disabled = !playable;
  };
  for (const [label, mode, duration] of [["Walk 20s", "walk", 20000], ["Mine 3s", "mine", 3000]]) {
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
  const stop = document.createElement("button");
  stop.textContent = "Stop probe";
  stop.onpointerdown = event => event.preventDefault();
  stop.onclick = () => finish("stopped");
  panel.append(stop);
  worker.addEventListener("message", event => {
    probe.observe(event.data);
    if (event.data.kind === "progress") {
      const phase = event.data.event?.phase;
      if (probe.playable && !playable) {
        playable = true;
        for (const button of buttons) button.disabled = !!probe.active;
        status.textContent = "Probe ready; aim at a block before mining";
      } else if (phase === "world-create-started" || phase === "world-open-started") {
        playable = false;
        finish("new-join");
      }
    } else if (event.data.kind === "error") {
      playable = false;
      finish("worker-error");
    }
  });
  window.addEventListener("pagehide", () => finish("pagehide"));
}
