# Block light-properties oracle

## What it is

`LightPropertiesOracle` queries every built-in block state's raw light dampening
and emission from an actual bootstrapped server registry. The harness contains no
release-specific state count, protocol number, or block-property corrections.

## How it works

The dump has a `C <state-count> <block-count>` header, one ascending
`<wire-state-id> <block-resource-name> <raw-dampening> <emission> <properties>`
row per state, and a matching `E` trailer. Properties are sorted by key and joined
with commas; `-` denotes a state with no properties. The comment header records
the queried server's version and protocol.

The harness verifies contiguous IDs, unique semantic states, values in `0..=15`,
and agreement between the global state registry and every block's state
definition. It buffers all rows until the census passes and redirects bootstrap
output to stderr. Consumers must still require the trailer to reject interrupted
writes, then join IDs and semantic signatures against that release's generated
`blocks.json`. Never interpret one release's numeric IDs through another
release's state table.

Both queried values are cached per state during registry initialization and take
no world or position argument. Raw dampening is the input consumed by
`lodestone_world::LightProperties::opacity`: ordinary spread applies its own
minimum cost of one, while skylight source detection needs the raw zero value.
Emission is the input consumed by `LightProperties::emission`.

These scalar values are not a complete light propagation oracle. Opposing faces
of adjacent states can jointly block a propagation edge, and vertical skylight
source detection also considers those faces. That decision depends on both
states and a direction. An exact scalar census cannot substitute for the shape
rule in a solver that only accepts one state's opacity.

## How to change it

Change the harness in `crates/lodestone-data/oracle-java/LightPropertiesOracle.java`
when the queried server's public registry API changes. Preserve direct queries,
full census checks, semantic signatures, and the completion trailer. Recompile
against each release being compared; successful compilation against one jar
does not verify another.

After capture, independently compare every row's ID, name, and properties with
the generated report. Use lit/unlit furnaces, several `light[level=...]` states,
berries on/off cave vines, glow lichen, wet/dry slabs, tinted glass, water, and
leaves as discriminating witnesses. A truncation and a duplicate or changed
semantic row must make the consumer fail before publishing data. Run the
controls and retain their failures; proposing them is not verification.

## Configuration

Run from the repository root after obtaining a quiet machine window. Capture
releases serially, with the JVM and container bounded independently. This
example writes private raw data to a new temporary directory:

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

Require exit status zero, a complete trailer, report identity agreement, and
successful negative controls before using the capture. Keep the raw report IDs
private until the consuming generator has mapped them to semantic states.

This independent report comparison also exercises interrupted-write, shifted-ID,
and changed-name controls without modifying the original capture:

```sh
python3 - "$task_cache/generated/reports/blocks.json" "$task_capture/light-properties.raw" <<'PY'
import json
import sys
from pathlib import Path

report = json.loads(Path(sys.argv[1]).read_text())
expected = {}
for name, block in report.items():
    for state in block['states']:
        props = ','.join(k + '=' + v for k, v in sorted(state.get('properties', {}).items())) or '-'
        assert state['id'] not in expected
        expected[state['id']] = (name, props)
assert set(expected) == set(range(len(expected)))
records = [line.split() for line in Path(sys.argv[2]).read_text().splitlines()
           if line.strip() and not line.startswith('#')]

def validate(rows):
    assert rows[0] == ['C', str(len(expected)), str(len(report))], 'header mismatch'
    assert rows[-1] == ['E', str(len(expected)), str(len(report))], 'completion trailer mismatch'
    assert len(rows) == len(expected) + 2, 'state count mismatch'
    for index, row in enumerate(rows[1:-1]):
        assert len(row) == 5, ('column count', index)
        raw, name, dampening, emission, props = row
        assert int(raw) == index, ('state id mismatch', index, raw)
        assert (name, props) == expected[index], ('semantic mismatch', index, name, props)
        assert 0 <= int(dampening) <= 15 and 0 <= int(emission) <= 15

validate(records)
print('PASS', len(expected), 'states', len(report), 'blocks')
shifted = [row[:] for row in records]
shifted[1][0] = '1'
renamed = [row[:] for row in records]
renamed[1][1] = 'control:incorrect_block'
for label, rows in [('truncated', records[:-1]), ('shifted-id', shifted), ('changed-name', renamed)]:
    try:
        validate(rows)
    except AssertionError as error:
        print('REJECTED', label, error)
    else:
        raise AssertionError(label + ' control was accepted')
PY
```

## Dependencies

The extracted official server jar and its libraries for the chosen release,
JDK 25, and Apple `container` are required for capture. Generated `blocks.json`
provides the independent identity census. The harness boots registries without
starting a world or loading datapacks; downstream shape-sensitive propagation
requires separate solver support and behavioral verification.
