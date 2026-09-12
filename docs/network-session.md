# Shell network sessions

## What it is

The shell network session is the boundary between asynchronous protocol work and the synchronous
simulation/render loop. `lodestone_shell::net` keeps the public façade and session lifecycle, while
its focused submodules own forwarded events, latest-value state, and browser integrated-server transport.

## How it works

The native and browser clients use the same `NetClient` lifecycle and `ClientAction` relay. A background
driver decodes client events and folds them into `NetUpdate` values for the simulation to drain. Values
where only the newest snapshot matters—weather, biome metadata, command suggestions, sky-light defaults,
and resource-pack prompts—use shared cells in `net/state.rs` instead of adding queue traffic.

Browser singleplayer starts its integrated server in a Worker. `net/browser.rs` translates the Worker
`MessagePort` into the client transport and waits for an explicit startup-ready response; startup errors
and post-start crashes remain distinct so a failed launch cannot create a second world owner.

## How to change it

Keep `net.rs` as the compatibility façade: public types moved into a submodule must be re-exported there,
and callers should continue to use `lodestone_shell::net` paths. Add replayable simulation inputs to
`net/events.rs`; add latest-value, lock-free or snapshot state to `net/state.rs`. Browser Worker control
messages and transport shutdown belong in `net/browser.rs`. Preserve the bounded relay behavior and the
native/wasm transport seam when changing session setup.

## Configuration

Protocol selection is supplied as a protocol number and resolved by `lodestone-registry`. The browser
Worker uses the bundled `lodestone-server-worker.js` endpoint. Relay capacities and native LAN/persistence
options remain constants or fields in `lodestone_shell::net`.

## Dependencies

- `lodestone-client` for the version-free client handle, events, actions, and transport builder.
- `lodestone-registry` for protocol-number adapter resolution.
- `lodestone-server` for integrated sessions and native LAN publication.
- `lodestone-net`, `wasm-bindgen`, and `web-sys` for the browser Worker transport.
- `lodestone-shell::sim` and render/menu consumers for the forwarded events and shared state.
