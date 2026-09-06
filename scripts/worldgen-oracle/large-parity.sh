#!/usr/bin/env bash
# Run one bounded shard. The full target is --cx -500 500 --cz -500 500.
set -euo pipefail
"$(cd "$(dirname "$0")" && pwd)/run.sh" LargeParityOracle "$@"
