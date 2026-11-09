#!/usr/bin/env bash
# Adds the SDK libraries libgit2 needs on iOS (zlib, iconv) to the Xcode
# project `cargo tauri ios init` generates, then regenerates it. Tauri's config
# only takes frameworks, and `cargo tauri ios build` drops XCODE_XCCONFIG_FILE
# from xcodebuild's environment, so the project spec is the one place left.
# Run after every `cargo tauri ios init`; running it twice changes nothing.
set -euo pipefail

spec="$(dirname "$0")/../crates/app/gen/apple/project.yml"
if [ ! -f "$spec" ]; then
  echo "no $spec: run \`cargo tauri ios init\` first" >&2
  exit 1
fi

if ! grep -q 'sdk: libz.tbd' "$spec"; then
  anchor='      - sdk: WebKit.framework'
  grep -qxF "$anchor" "$spec" || { echo "WebKit dependency not found in $spec" >&2; exit 1; }
  awk -v anchor="$anchor" '
    { print }
    $0 == anchor { print "      - sdk: libz.tbd"; print "      - sdk: libiconv.tbd" }
  ' "$spec" > "$spec.tmp"
  mv "$spec.tmp" "$spec"
fi

xcodegen generate --spec "$spec"
