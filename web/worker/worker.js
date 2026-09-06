// Worker control plane for browser singleplayer. Protocol bytes never use this
// channel: they use the transferred MessagePort after the server is ready.
self.onmessage = async (event) => {
  const request = event.data;
  const port = event.ports[0];
  if (request?.kind !== "start" || !port) {
    self.postMessage({ kind: "error", message: "invalid server worker launch" });
    return;
  }
  try {
    const wasm = await import("./lodestone-server-worker-wasm.js");
    await wasm.default();
    wasm.start_worker(port, request.protocol, BigInt(request.seed), request.preset);
    self.postMessage({ kind: "ready" });
  } catch (error) {
    self.postMessage({ kind: "error", message: String(error) });
  }
};
