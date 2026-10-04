# Session game data

## What it is

Each client session owns one version adapter shared by packet decoding, ECS gameplay queries, collision, and selection outlines. Server-synchronized block tags belong to that session, not to a process-wide data table.

## How it works

`ClientBuilder` converts its selected adapter into an `Arc` and installs that same handle in the driver, `ClientHandle`, and the read-model's `VersionData` ECS resource. The shell obtains collision facts from that resource and outline facts from the client handle. Ending a shell session clears its resource. A feature-enabled adapter is never inferred as the live session's data source.

`GameDataVersion` selects release-specific static facts and wire-to-canonical identity maps. Canonical state storage is distinct from a release's wire palette. Queries reject states absent from the selected release. Fluid collision asks for the fluid-blocking predicate; the older generic motion predicate is not substituted for a newer release's purpose-specific data.

The adapter decodes synchronized tags into a temporary canonical `BlockTagSnapshot`. Counts, keys, duplicate registries and tags, member identities, and complete packet consumption are checked before publication. An invalid packet leaves the previous snapshot intact. A present block registry replaces its entire snapshot, including an empty replacement; an absent block registry leaves it unchanged.

The adapter replaces one `Arc` under a short `RwLock`. A mining query clones that snapshot handle once, then evaluates all rules without holding the lock or cloning membership tables. Before the first snapshot, mining uses the selected release's built-in tags. Once installed, a missing tag matches nothing. Explicit wire tool-rule members are translated through the selected release's block map when evaluated; generated rules already use canonical members.

Movement properties use `GameDataVersion::movement` and `VersionAdapter::block_movement`. The numeric table carries friction, speed and jump factors, restitution, climbability, and bounce suppression; synchronized session tags replace the last two membership facts. Restitution is suppressed only after reading both independent fields. The complete-data capability makes an unsupported state an explicit unknown, never an invitation to select another release. Older adapters without that capability retain a bounded canonical-prefix compatibility path.

The movement census does not authenticate imperative stuck multipliers. The shell keeps the existing cobweb, powder-snow, and berry-bush rules separate; extending those requires behavioral evidence, not another numeric movement row. The movement tests compare every old captured row and verify shelf-mushroom restitution through an asset-free live collision view, with separate suppression and rejected-state controls.

Teleport completion also uses the retained adapter: the driver supplies the pose actually adopted by the consumer when finalizing a release-specific acknowledgement body.

## How to change it

Extend static facts in `lodestone-data` and expose purpose-specific queries through `VersionAdapter`; keep the version-free model independent of the data crate. Add a new session query through the existing shared handle rather than constructing another adapter. Update tag tests in the connection adapter and identity tests in `lodestone-client::state` when changing ownership or publication.

The rollback tests include a valid replacement with a trailing byte and a later invalid registry. The isolation test updates two adapters independently and queries through a retained handle. These are distinct from live joining and rendering acceptance tests.

## Configuration

The negotiated protocol selects the data profile. Registry feature flags only determine which families can be selected; they do not select process-global gameplay data. There are no environment variables for tag publication.

## Dependencies

`lodestone-client` owns driver and read-model lifetimes; `lodestone-ecs` stores the shared adapter; `lodestone-model` defines the version-free query seam; version crates validate wire input; `lodestone-data` owns canonical identities, static profiles, and immutable tag evaluation. The shell consumes the same session handle.
