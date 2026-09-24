// Worker control plane for browser singleplayer. Protocol bytes never use this
// channel: they use the transferred MessagePort after the server is ready.
// `importScripts` keeps this a classic Worker, which has wider support than a
// module Worker and still lets the browser load the wasm glue asynchronously.
importScripts("./lodestone-server-worker-bootstrap.js");

self.onmessage = (event) => {
  if (event.data?.kind === "cancel") {
    self.LodestoneWorkerBootstrap.cancel(event, (message) => self.postMessage(message));
    return;
  }
  self.LodestoneWorkerBootstrap.launch(
    event,
    (mode) => import(`./lodestone-server-worker-wasm-${mode}.js`),
    (message) => self.postMessage(message),
  );
};
