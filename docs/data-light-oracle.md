# Block light-properties oracle

## What it is

`LightPropertiesOracle` queries every built-in block state's raw light dampening and emission from an actually bootstrapped server registry. The harness has no release-specific state count, protocol number or block-property corrections.

## How it works

- The dump has a `C <state-count> <block-count>` header, one ascending `<wire-state-id> <block-resource-name> <raw-dampening> <emission> <properties>` row per state, and a matching `E` trailer. Properties are sorted by key and comma-joined (`-` for none). The comment header records the server's version and protocol.
- The harness verifies contiguous IDs, unique semantic states, values in `0..=15` and agreement between the global state registry and each block's state definitions, buffers rows until the census passes, and sends bootstrap output to stderr. Consumers must still require the trailer (to reject interrupted writes) and join IDs and semantic signatures against that release's generated `blocks.json`; never read one release's IDs through another's table.
- Both values are cached per state and take no world or position. Raw dampening feeds `lodestone_world::LightProperties::opacity` (spread applies its own minimum cost of one, while skylight source detection needs the raw zero); emission feeds `LightProperties::emission`.
- These scalars are not a full propagation oracle: opposing faces of adjacent states can jointly block an edge, and skylight source detection also uses those faces. That depends on two states and a direction, so a scalar census cannot replace the shape rule in a solver that accepts one state's opacity.

## How to change it

- Edit `crates/lodestone-data/oracle-java/LightPropertiesOracle.java` when the server's public registry API changes, preserving direct queries, the full census, semantic signatures and the trailer. Recompile per release; compiling against one jar proves nothing about another.
- After capture, compare every row's ID, name and properties with the generated report, using discriminating witnesses: lit and unlit furnaces, several `light[level=...]` states, berries on and off cave vines, glow lichen, wet and dry slabs, tinted glass, water and leaves. A truncation, or a duplicate or changed semantic row, must make the consumer fail before publishing. Run the controls and keep their failures.
- The independent comparison builds the expected `id -> (name, properties)` map from `blocks.json` (ids contiguous from zero), validates header, trailer, row count, row id order, `(name, props)` and ranges, then must reject three corrupted copies: truncated, a shifted first id and a renamed block.

## Configuration

Capture releases serially on a quiet machine, bounding JVM and container independently:

```sh
task_version=26.3
task_capture="$(mktemp -d /private/tmp/lodestone-light-properties.XXXXXX)"
task_cache="$PWD/.cache/mc/$task_version"
task_oracles="$PWD/crates/lodestone-data/oracle-java"
container run --rm --memory 3g --cpus 2 \
  -v "$task_cache":/mc:ro -v "$task_oracles":/oracle:ro \
  -e MC_VERSION="$task_version" -w /work eclipse-temurin:25-jdk bash -euc '
    CP="/mc/versions/$MC_VERSION/server-$MC_VERSION.jar"
    while IFS= read -r jar; do CP="$CP:$jar"; done < <(find /mc/libraries -name "*.jar" | sort)
    javac -J-Xmx512m -cp "$CP" -d /work /oracle/LightPropertiesOracle.java
    java -Xmx2g -XX:ActiveProcessorCount=2 -cp "/work:$CP" LightPropertiesOracle
  ' > "$task_capture/light-properties.raw" 2> "$task_capture/capture.log"
```

Require exit status zero, a complete trailer, identity agreement with `$task_cache/generated/reports/blocks.json` and rejected negative controls before using the capture, and keep raw IDs private until the consuming generator maps them to semantic states.

## Dependencies

The extracted official server jar and libraries, JDK 25 and Apple `container` for capture; generated `blocks.json` as the independent identity census. The harness boots registries without a world or datapacks.
