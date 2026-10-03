#!/usr/bin/env bash
# Build everything this machine can build. Cross-targets are added as they come online.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked --offline --workspace
