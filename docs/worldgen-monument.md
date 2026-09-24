# Ocean monument room graph

## What it is

The ocean monument generator builds one fixed shell plus an internal room graph. The graph stage creates the 46-cell arena, wires the special entry/core/roof/wing nodes, and selects a shuffled set of room footprints for the later block-writing stage.

## How it works

`structure::monument::graph` owns graph construction and opening closure. It stores rooms in an arena so connections remain stable while the graph is edited. The construction pass walks the fixed 5×3×5 coordinate space, adds bidirectional edges, claims the entry and eight-cell core, then shuffles the remaining room definitions. The closing pass attempts to remove up to two openings per room; each candidate edge is retained only when both sides can still reach the source room.

The parent monument module consumes the returned arena and ordered room definitions. Its room-fit cascade claims contiguous cells as double-wide, double-tall, deep, or simple rooms, and the existing geometry stage then writes the shell and selected pieces in dependency order.

## How to change it

Keep graph mutations in `crates/lodestone-worldgen/src/structure/monument/graph.rs` and preserve the returned arena indices: room builders use those indices to inspect openings and neighbouring rooms. Changes to edge orientation, pre-shuffle order, or the short-circuiting reachability check alter the random stream and room partition, so update the graph self-consistency tests with any intentional algorithm change.

The parent module owns the shared six-direction constants and coordinate helpers. If the room layout changes, update both the graph's fixed-cell walk and the room bounding-box calculations in `monument.rs` together.

## Configuration

There are no monument-specific flags or environment variables. The structure seed, chunk coordinates, and `StartContext` determine the generated pieces; the graph itself consumes only the structure random stream.

## Dependencies

The graph uses `lodestone_worldgen_core::rng::RandomSource` and the parent monument module's direction and coordinate helpers. The parent later consumes its `RoomDef` arena through `Canvas`, `StructurePiece`, and the shared `StartContext` terrain interface.
