# Lodestone

Lodestone is an independent Minecraft Java Edition client and integrated server written from
scratch in Rust (edition 2024) with wgpu. `lodestone-client` is the reusable client library;
the `lodestone` binary is its interactive native shell.

Work in progress. Gameplay, rendering, and world generation still have bugs and differences
from the original game.

## Current scope

The workspace contains the client library, native shell, integrated server, renderer,
world-generation code, protocol adapters, and optional plugins. The native shell's default
features enable the `v26-2` adapter for Minecraft 26.2 (protocol 776), plus multiplayer and
windowed presentation. Older protocol adapters are opt-in; joining and hosting support differ.

There is also a separate browser build for `wasm32-unknown-unknown`. It shares the shell and
renderer but has its own launch, asset, and runtime requirements; see
[`web/README.md`](./web/README.md) and [`docs/browser-shell-port.md`](./docs/browser-shell-port.md).

## Quick start

Install the Rust toolchain specified in [`rust-toolchain.toml`](./rust-toolchain.toml) and
[`just`](https://github.com/casey/just). Then run:

```sh
just run                                # build and open the native shell
just run --host example.org --port 25565  # connect on launch
just health                             # workspace checks, tests, and comment lint
```

`just run-wasm` starts the browser development loop at `http://127.0.0.1:8080/`. It additionally
requires `trunk` and the `wasm32-unknown-unknown` target; the launcher reports missing
prerequisites. Browser assets and runtime constraints are documented in
[`web/README.md`](./web/README.md).

## License

Lodestone-owned code is licensed under the
[GNU General Public License, version 3 or later](./LICENSE) (`GPL-3.0-or-later`).

Lodestone is not affiliated with Mojang Studios or Microsoft. See
[`docs/legal-notices.md`](./docs/legal-notices.md) for attribution and licensing details.

## AI use

This project is developed with AI assistance. Generated code can contain mistakes;
review and test it before relying on it.

## Reading further

- [`docs/architecture.md`](./docs/architecture.md) — crate structure and cross-cutting constraints
- [`docs/multi-protocol-seam.md`](./docs/multi-protocol-seam.md) — protocol-family selection
- [`docs/repo-tooling.md`](./docs/repo-tooling.md) — build and test commands
- [`docs/README.md`](./docs/README.md) — subsystem documentation index
