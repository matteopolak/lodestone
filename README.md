# Lodestone

Lodestone is an unofficial Minecraft Java Edition client written in Rust. You can
play singleplayer or connect to multiplayer servers. The default build targets Minecraft 26.2.

Work in progress. Gameplay, rendering, and world generation still have bugs and differences
from the original game.

## Screenshots

Captured in Lodestone while connected to a Minecraft 26.2 server.

| | |
|---|---|
| ![Text displays](./docs/images/01-text-displays.png) | ![Signs](./docs/images/02-signs.png) |
| ![Banners, chests, and other block entities](./docs/images/03-block-entities.png) | ![Mobs and armour stands](./docs/images/04-entities.png) |

![In-game HUD and chat](./docs/images/05-hud.png)

## Play

There are no prebuilt releases yet. To build and launch the client, install
[Rust](https://rustup.rs/) and [`just`](https://github.com/casey/just), then run:

```sh
git clone https://github.com/matteopolak/lodestone.git
cd lodestone
cargo run -p xtask -- fetch-assets --version 26.2
just run
```

Rust uses the toolchain pinned in this repository. The first build may take a while.
Once the client opens, choose singleplayer or multiplayer from the main menu.
On Debian or Ubuntu, install the build dependencies first with
`sudo apt install libasound2-dev pkg-config`.

You can also connect directly when launching:

```sh
just run --host example.org --port 25565
```

For the experimental browser version, see the [browser setup instructions](./web/README.md).
Please report bugs through [GitHub issues](https://github.com/matteopolak/lodestone/issues).

## License

Lodestone-owned code is licensed under the
[GNU General Public License, version 3 or later](./LICENSE) (`GPL-3.0-or-later`).

Lodestone is not affiliated with Mojang Studios or Microsoft. See
[`docs/legal-notices.md`](./docs/legal-notices.md) for attribution and licensing details.

## AI use

AI is used extensively to develop this project. Bugs and incomplete features remain.
