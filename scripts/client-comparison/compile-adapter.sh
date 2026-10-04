#!/usr/bin/env bash
# Builds two private adapter jars from adapter/ into $1 (fresh dir): lodestone-bench-263.jar (needs Sodium) and lodestone-bench-263-vanilla.jar.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
R=${LODESTONE_COMPARISON_ROOT:-/Volumes/LodestoneScratch/java-26.3}
JB=$(dirname "${LODESTONE_COMPARISON_JAVA:-$HOME/Library/Application Support/minecraft/runtime/java-runtime-epsilon/mac-os-arm64/java-runtime-epsilon/jre.bundle/Contents/Home/bin/java}")
SRC=$HERE/adapter
OUT=${1:?fresh output dir}
[[ -e $OUT ]] && { echo "exists: $OUT" >&2; exit 2; }
CP=$(find "$R/libraries" "$R/fabric-libraries" -name '*.jar' | tr '\n' ':')$R/minecraft/26.3-client.jar:$R/modsets/optimized/sodium-fabric-0.9.2+mc26.3.jar
mkdir -p "$OUT/classes-sodium" "$OUT/classes-vanilla" "$OUT/res-sodium" "$OUT/res-vanilla"
"$JB/javac" --release 25 -proc:none -cp "$CP" -d "$OUT/classes-sodium" $(find "$SRC/bench" -name '*.java')
"$JB/javac" --release 25 -proc:none -cp "$CP" -d "$OUT/classes-vanilla" $(find "$SRC/bench" -name '*.java' ! -name SodiumProbe.java ! -name '*Accessor.java') "$HERE/adapter-vanilla-stub/bench/SodiumProbe.java"
cp "$SRC/fabric.mod.json" "$SRC/bench.mixins.json" "$SRC/bench-sodium.mixins.json" "$OUT/res-sodium/"
cp "$SRC/fabric.mod.vanilla.json" "$OUT/res-vanilla/fabric.mod.json"; cp "$SRC/bench.mixins.json" "$OUT/res-vanilla/"
"$JB/jar" --create --file "$OUT/lodestone-bench-263.jar" -C "$OUT/classes-sodium" . -C "$OUT/res-sodium" .
"$JB/jar" --create --file "$OUT/lodestone-bench-263-vanilla.jar" -C "$OUT/classes-vanilla" . -C "$OUT/res-vanilla" .
shasum -a 256 "$OUT"/*.jar
