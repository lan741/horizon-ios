#!/usr/bin/env bash
#
# swift_typecheck.sh — the CI/Linux twin of scripts/swift_typecheck.ps1.
#
# Same three levels, no PowerShell needed:
#   1. parse every Swift file
#   2. type check the Foundation-only files against the real C header
#   3. type check the model layer against the Combine shim
#
# Needs a Swift toolchain on PATH (any platform: Linux, macOS, Windows via WSL).
# No Apple SDK, so the SwiftUI/Metal files are parse-only here; the macOS job
# builds them for real.
#
# Exit: 0 clean, 1 problems, 2 no toolchain.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/app"
INCLUDE="$ROOT/engine/include"
BRIDGING="$APP/HorizonPad/Bridge/HorizonPad-Bridging-Header.h"
SHIM="$ROOT/scripts/swift_shims/Combine.swift"
BUILD="$ROOT/.build/swiftshims"

if ! command -v swiftc >/dev/null 2>&1; then
  echo "no Swift toolchain on PATH; skipping"
  exit 2
fi

echo "swiftc: $(command -v swiftc)"
swiftc --version | head -n 1

# `mapfile` is a bash 4 builtin and macOS still ships bash 3.2, so read the list
# the portable way.
SOURCES=()
while IFS= read -r file; do
  SOURCES+=("$file")
done < <(find "$APP" -name '*.swift' | sort)
if [ "${#SOURCES[@]}" -eq 0 ]; then
  echo "no Swift sources found" >&2
  exit 1
fi
echo "sources: ${#SOURCES[@]}"

# A Windows toolchain keeps its SDK in a sibling Platforms directory; Linux and
# macOS hosts need no -sdk. Detect once and pass it everywhere.
SDK_ARGS=()
SWIFTC_REAL="$(command -v swiftc)"
dir="$(cd "$(dirname "$SWIFTC_REAL")" && pwd)"
for _ in 1 2 3 4 5 6; do
  if [ -d "$dir/Platforms" ]; then
    sdk="$(find "$dir/Platforms" -maxdepth 5 -type d -name 'Windows.sdk' 2>/dev/null | sed -n '1p')"
    if [ -n "${sdk:-}" ]; then
      echo "sdk: $sdk"
      SDK_ARGS=(-sdk "$sdk")
      break
    fi
  fi
  dir="$(dirname "$dir")"
  [ "$dir" = "/" ] && break
done

FAILED=0

step() {
  local label="$1"; shift
  echo
  echo "== $label =="
  if ! "$@"; then
    echo "FAILED: $label" >&2
    FAILED=1
  fi
}

# 1. parse --------------------------------------------------------------------
step "parse (${#SOURCES[@]} files)" \
  swiftc -parse -swift-version 5 ${SDK_ARGS[@]+"${SDK_ARGS[@]}"} "${SOURCES[@]}"

# 2. Foundation-only files, with the C header imported -------------------------
step "typecheck Foundation + hz_ffi" \
  swiftc -typecheck -swift-version 5 ${SDK_ARGS[@]+"${SDK_ARGS[@]}"} \
    -import-objc-header "$BRIDGING" \
    -Xcc "-I$INCLUDE" \
    "$APP/HorizonPad/Support/Paths.swift" \
    "$APP/HorizonPad/Engine/HZEngine.swift"

# 3. model layer ---------------------------------------------------------------
# On Apple hosts the real Combine framework exists, so the model layer is checked
# against it directly. Building the shim here would fail: a local module named
# `Combine` that imports `Foundation` is a circular dependency on macOS.
# The shim exists only for hosts without an Apple SDK (Windows, Linux).
if [ "$(uname -s)" = "Darwin" ]; then
  step "typecheck model layer (real Combine)" \
    swiftc -typecheck -swift-version 5 ${SDK_ARGS[@]+"${SDK_ARGS[@]}"} \
      "$APP/HorizonPad/Support/Paths.swift" \
      "$APP/HorizonPad/Model/Library.swift" \
      "$APP/HorizonPad/Model/Settings.swift"
else
  mkdir -p "$BUILD"
  if swiftc -emit-module -module-name Combine -swift-version 5 ${SDK_ARGS[@]+"${SDK_ARGS[@]}"} \
      -emit-module-path "$BUILD/Combine.swiftmodule" "$SHIM"; then
    step "typecheck model layer + Combine shim" \
      swiftc -typecheck -swift-version 5 ${SDK_ARGS[@]+"${SDK_ARGS[@]}"} -I "$BUILD" \
        "$APP/HorizonPad/Support/Paths.swift" \
        "$APP/HorizonPad/Model/Library.swift" \
        "$APP/HorizonPad/Model/Settings.swift"
  else
    echo "FAILED: build Combine shim" >&2
    FAILED=1
  fi
fi

echo
if [ "$FAILED" -ne 0 ]; then
  echo "swift checks FAILED" >&2
  exit 1
fi
echo "swift checks passed"