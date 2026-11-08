#!/usr/bin/env bash
# Headless Chromium checks against the mocked UI: the IPC escape probe,
# artifact placement + the view bridge, and the sync UI (see run.js).
# Bun and Chromium come from the flake's pinned nixpkgs; neither is a
# dependency of the app. Usage: scripts/ui-check/run.sh [ipc|placement|sync]...
set -euo pipefail
cd "$(dirname "$0")/../.."
exec nix shell --inputs-from . nixpkgs#bun nixpkgs#chromium --command bun scripts/ui-check/run.js "$@"
