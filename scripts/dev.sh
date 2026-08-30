#!/bin/sh
# Rebuild everything on the current sources and restart the agent, so the
# CLI, the launchd agent and the desktop sidecar all carry the same build
# stamp (the CLI and the app refuse to talk to an agent from another build).
#
#   scripts/dev.sh            # then: cd apps/desktop && npm run tauri dev
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
./target/debug/filemind agent stop 2>/dev/null || true
cargo build --workspace
sh scripts/build-sidecar.sh --debug
sleep 1
./target/debug/filemind agent install
sleep 3
./target/debug/filemind agent status
echo
echo "agent restarted on the current build. If the app is running, restart it too:"
echo "  cd apps/desktop && npm run tauri dev"
