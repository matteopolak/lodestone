# Paper world bridge

## What it is

`lodestone-jvm-bridge` exposes a deliberately small, loader-local world and block surface to an operator-built `lodestone.bridge.IsolatedPaperShim`: resident block-state reads and replacements plus ordered bulk operations. It is not a general server, world, chunk or block-object API.

## How it works

- `native_surface::isolated_shim_methods` is the source of truth for every supported native member and JNI descriptor. `native_surface::paper_world_surface_census` maps each world and block declaration to a finite Rust producer category, and an aligned capability table is checked against every shim declaration, so a member cannot be unlisted or presented as supported without a resident read, batch read, write, callback observation or worker-local handle. Registration validates every declared member on the shim before installing callbacks; a missing or wrong one fails setup naming class, method and descriptor.
- Scalar `blockStateId(int,int,int)` and `setBlockStateId(int,int,int,int)` cross the bounded `WorldPort` request/response seam. The dedicated host touches only already resident primary-world blocks; unavailable columns, out-of-height coordinates and invalid state IDs are named errors, never generated terrain or a sentinel air. State IDs are generated `StateId` values, validated by the host.
- `blockStateIds(int[])`: ordered `(x,y,z)` triples in, one state ID per triple out in the same order. At most 4,096 positions; one batch request, copied in and out; serviced after the scalar port and before any Java callback dispatch, so a region read is one JNI and one port crossing.
- `setBlockStateIds(int[])`: ordered `(x,y,z,stateId)` quadruples, returning the applied count. At most 64 replacements, because each reserves an ordered change-observer callback. The host validates every state ID and residency first; any invalid state, unavailable position or insufficient callback capacity rejects the whole batch with no partial mutation. A success applies in order and queues the same per-block notifications as scalar writes.
- `setBlockStateIdsWithFlags(int[],int)`: bit `0x01` queues the resident-change listener callback, `0` does the same validated write without it. Any other bit (physics, neighbour propagation, packets, block entities) fails by name before the host sees the batch. The unflagged method is fixed to `0x01`.
- The dedicated host routes scalar and batch writes through the server's proposal bus, so native-plugin adjudication can deny or replace a request before success is reported. The batch is one action (ordered replacements plus notification policy), preflighted before the first mutation, and returns the adjudicated batch; refusals are finite `ColumnNotResident` and `OutOfBounds`. A batch is never split into scalar proposals, which could apply partially after a later denial.
- Nothing shares an ECS value, world object, lock guard or pointer with Java. Callbacks run on the adapter worker while the tick owner services copied request values. Block handles for the listener subset are generation-checked worker-local values holding only owner identity and coordinates; a released or malformed handle fails before any host read or write.
- Tests: the ignored `paper_lifecycle::plugin_child_reads_and_writes_resident_block_state_through_worker_ports` uses repository-owned stand-in archives over an in-memory state map with a real JDK and JNI (scalar and batch writes, both notification policies, read-after-write, a malformed handle failing with a named lifetime error); a separate adapter-host JDK fixture covers a stale player handle after disconnect and slot reuse. They validate JNI and worker ownership, not Paper compatibility.

## How to change it

- Add a member by extending `native_surface::ISOLATED_SHIM_METHODS`, its validation and registration dispatch, and the adapter callback together, plus the producer in `java_adapter::JavaAdapter::poll`; a member with no producer is unsupported and must not be listed. Keep a hermetic `AdapterHost` test for ordering, bounds and port-request count, and an ignored fresh-process JDK fixture compiling the exact Java declaration.
- Reads keep the 4,096 cap, order and exact response length. Writes keep the 64 cap whenever `0x01` is set and preflight the whole request first. Add no update bit until the host has an observable producer for it. Never turn a missing column into a load or generate request, and never put a world or ECS handle in `WorldPort`.

## Configuration

Needs the dedicated server's default-off `jvm` feature, `LODESTONE_JAVA_ADAPTER_CLASS`, `LODESTONE_JAVA_CLASSPATH` and an operator-built shim via `LODESTONE_PAPER_SHIM_PATH`. `LODESTONE_JAVA_DEADLINE_MS` bounds startup and each callback. Nothing relaxes resident-only access.

## Dependencies

`lodestone-jvm-bridge`'s `adapter`, `native_surface` and `port` modules. The only production producer is `lodestone-dedicated-server`'s `JavaAdapter`, which reads `IntegratedServer` through its resident-block methods and validates writes against `lodestone-data`'s block-state table.
