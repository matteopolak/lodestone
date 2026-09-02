#!/usr/bin/env bash
#
# run.sh — execute the classload-interception spike in an ephemeral JDK
# container.
#
# WHY A CONTAINER
#   This host has no Java runtime (`java -version` reports "Unable to locate a
#   Java Runtime"), and installing one is not sanctioned. Every JVM oracle in
#   this repo runs under Apple `container` instead, with the JVM coming from the
#   image — see docs/oracle-runtimes.md, and scripts/worldgen-oracle/run.sh for
#   the same shape.
#
# WHAT IT PROVES
#   See Spike.java's class documentation. In one sentence: `org.example.Caller`
#   is compiled ONCE against the real NMS signature and then loaded through two
#   loaders differing in one path element; if the answer changes, classload
#   interception works on already-compiled third-party bytecode, which is the
#   single mechanism the bridge design rests on. The control arm — which must
#   NOT show interception — is what makes the test arm meaningful.
#
#   Note this spike deliberately uses stand-in classes rather than real Paper
#   bytecode. That is the point: it isolates the *mechanism*, and it keeps the
#   spike runnable by anyone without a Paper jar. It does not, and does not
#   claim to, prove that Paper's ~7,000-member surface is shimmable — that is
#   what the census sizes.
#
# USAGE
#   ./crates/plugins/lodestone-jvm-bridge/spike/run.sh

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

container system start >/dev/null 2>&1 || true

container run --rm \
  --memory 2g \
  -v "${HERE}":/spike:ro \
  -w /work \
  eclipse-temurin:25-jdk \
  bash -c '
    set -e
    mkdir -p /work/real /work/shim /work/app /work/harness

    # `Caller` is compiled against the REAL Level, exactly once. Nothing
    # recompiles it for the shim arm -- if it had to be recompiled, the design
    # would be jar patching rather than classload interception.
    javac -d /work/real $(find /spike/real -name "*.java")
    javac -d /work/shim $(find /spike/shim -name "*.java")
    javac -cp /work/real -d /work/app $(find /spike/app -name "*.java")
    javac -d /work/harness /spike/Spike.java

    java -cp /work/harness Spike /work/real /work/shim /work/app
  '
