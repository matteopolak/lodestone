# Paper world bridge

## What it is

`lodestone-jvm-bridge` exposes a deliberately small, loader-local world/block surface to an operator-built `lodestone.bridge.IsolatedPaperShim`. It supplies resident block-state reads and replacements plus ordered bulk reads; it is not a general server, world, chunk, or block-object API.

## How it works

`native_surface::isolated_shim_methods` is the generated source of truth for every supported native member, including its JNI descriptor. `native_surface::paper_world_surface_census` maps each implemented world/block declaration to its finite Rust producer category, so a declaration cannot be presented as supported without a resident read, batch-read, write, callback-observation, or worker-local handle capability. Registration first validates every declared member on the shim and only then installs the matching Rust callbacks. A missing or wrong declaration fails setup with the class, method, and descriptor in the error.

Scalar `blockStateId(int,int,int)` and `setBlockStateId(int,int,int,int)` cross the bounded `WorldPort` request/response seam. The dedicated host reads or replaces only an already resident primary-world block; unavailable columns, out-of-height coordinates, and invalid state IDs are named errors, never generated terrain or a sentinel air state. State identifiers are the server's generated `StateId` values and remain `int` values only after the host validates them.

`blockStateIds(int[])` is the bulk-read path. Its input is ordered `(x,y,z)` triples and its result has one state ID per triple in the same order. It accepts at most 4,096 positions, copies the Java array, sends exactly one batch request to the host, and copies the response into a fresh Java array. The host services the ordered batch after the scalar port and before any Java callback dispatch. This makes region reads one JNI and one port crossing rather than one crossing per block, while still performing each resident-world read on the host side.

`setBlockStateIds(int[])` is the matching ordered bulk-write path. Its input is `(x,y,z,stateId)` quadruples and it returns the number of replacements applied. It accepts at most 64 replacements because each accepted replacement reserves one ordered change-observer callback. Before applying any replacement, the dedicated host validates every state ID and confirms every target is resident; an invalid state, unavailable position, or insufficient callback capacity rejects the whole batch without a partial mutation. A successful batch is then applied in input order and queues the same host-confirmed per-block notifications as scalar writes.

The bridge never shares an ECS value, world object, lock guard, or pointer with Java. Java callbacks run on the dedicated adapter worker; the tick owner services copied request values. Block handles used by the change-listener subset are generation-checked worker-local values containing only owner identity and coordinates. A released or malformed handle fails before a host read or write is queued.

## How to change it

Add a world-domain member by extending `native_surface::ISOLATED_SHIM_METHODS`, its validation and registration dispatch, and the matching adapter callback together. Add the public dedicated-host producer in `java_adapter::JavaAdapter::poll`; a bridge method with no producer is unsupported and must not be listed. Keep a hermetic `AdapterHost` test that proves ordering, bounds, and the number of port requests, plus an ignored fresh-process JDK fixture that compiles the precise Java declaration and calls it through JNI.

For batch reads, retain the 4,096-position cap, input ordering, and exact response-length check. For batch writes, retain the 64-replacement observer-capacity limit and preflight the entire request before the first mutation. Do not turn a missing column into a load/generate request, and do not add a world or ECS handle to `WorldPort`.

## Configuration

The native surface requires the dedicated server's default-off `jvm` feature, `LODESTONE_JAVA_ADAPTER_CLASS`, `LODESTONE_JAVA_CLASSPATH`, and an operator-built shim path selected through `LODESTONE_PAPER_SHIM_PATH`. `LODESTONE_JAVA_DEADLINE_MS` bounds startup and each callback. No switch relaxes resident-only reads or writes.

## Dependencies

The surface depends on `lodestone-jvm-bridge`'s `adapter`, `native_surface`, and bounded `port` modules. The only production world producer is `lodestone-dedicated-server`'s `JavaAdapter`, which reads `IntegratedServer` through its public resident-block methods and validates writes through `lodestone-data`'s generated block-state table.
