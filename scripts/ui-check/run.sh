#!/usr/bin/env bash
# Headless Chromium checks of plugin views against the mocked UI: the IPC
# escape probe and artifact placement + the view bridge (see run.js).
# Bun and Chromium come from the flake's pinned nixpkgs; neither is a
# dependency of the app. Usage: scripts/ui-check/run.sh [ipc|placement]...
set -euo pipefail
cd "$(dirname "$0")/../.."
exec nix shell --inputs-from . nixpkgs#bun nixpkgs#chromium --command bun scripts/ui-check/run.js "$@"
