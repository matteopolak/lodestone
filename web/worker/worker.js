// Worker control plane for browser singleplayer. Protocol bytes never use this
// channel: they use the transferred MessagePort after the server is ready.
// `importScripts` keeps this a classic Worker, which has wider support than a
// module Worker and still lets the browser load the wasm glue asynchronously.
importScripts("./lodestone-server-worker-bootstrap.js");

self.onmessage = (event) => self.LodestoneWorkerBootstrap.launch(
  event,
  () => import("./lodestone-server-worker-wasm.js"),
  (message) => self.postMessage(message),
);
