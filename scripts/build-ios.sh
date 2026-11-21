#!/usr/bin/env bash
#
# build-ios.sh — cross-compile the Rust engine for iPad and stage the archive
# where Xcode can find it.
#
#   ./scripts/build-ios.sh              # release, device + simulator
#   ./scripts/build-ios.sh --debug      # debug build (faster, bigger)
#
# Output: engine/target/ios/libhz_ffi.a  (universal device archive)
#         engine/target/ios-sim/libhz_ffi.a (simulator archive)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="$ROOT/engine"
PROFILE="release"
CARGO_FLAGS=("--release")

if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  CARGO_FLAGS=()
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found; install Rust from https://rustup.rs" >&2
  exit 1
fi

if ! xcodebuild -version >/dev/null 2>&1; then
  echo "Xcode command line tools are required" >&2
  exit 1
fi

echo "==> adding iOS targets"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim >/dev/null

echo "==> building for device (aarch64-apple-ios)"
cargo build "${CARGO_FLAGS[@]}" \
  --manifest-path "$ENGINE/Cargo.toml" \
  --target aarch64-apple-ios \
  -p hz-ffi

echo "==> building for simulator (aarch64-apple-ios-sim)"
cargo build "${CARGO_FLAGS[@]}" \
  --manifest-path "$ENGINE/Cargo.toml" \
  --target aarch64-apple-ios-sim \
  -p hz-ffi

mkdir -p "$ENGINE/target/ios" "$ENGINE/target/ios-sim"
cp "$ENGINE/target/aarch64-apple-ios/$PROFILE/libhz_ffi.a" "$ENGINE/target/ios/libhz_ffi.a"
cp "$ENGINE/target/aarch64-apple-ios-sim/$PROFILE/libhz_ffi.a" "$ENGINE/target/ios-sim/libhz_ffi.a"

# Strip debug symbols from the shipped archive; the dSYM-less engine reports
# through its own error buffer instead.
if command -v xcrun >/dev/null 2>&1; then
  xcrun strip -S "$ENGINE/target/ios/libhz_ffi.a" || true
fi

echo
echo "staged:"
ls -lh "$ENGINE/target/ios/libhz_ffi.a" "$ENGINE/target/ios-sim/libhz_ffi.a"
echo
echo "next:"
echo "  cd app && xcodegen generate && open HorizonPad.xcodeproj"