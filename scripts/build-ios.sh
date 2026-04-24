#!/usr/bin/env bash
#
# Build PimCore.xcframework from pim-ios-ffi for iOS device + simulator.
# Resolves milestone 1 of issue #70.
#
# Steps:
#   1. Preflight: xcode-select, rustup targets, required tools.
#   2. cargo build --release for aarch64-apple-ios, aarch64-apple-ios-sim,
#      x86_64-apple-ios.
#   3. lipo -create the two simulator slices into one fat simulator lib.
#   4. xcodebuild -create-xcframework with the device lib + fat sim lib
#      + the pim_ios_ffi.h header, producing target/ios/PimCore.xcframework.
#
# Usage:
#   scripts/build-ios.sh
#
# Environment:
#   Requires full Xcode (not just Command Line Tools) for step 4.
#   Run `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`
#   after installing Xcode from the App Store.

set -euo pipefail

cd "$(dirname "$0")/.."

PKG="pim-ios-ffi"
LIB="libpim_ios_ffi.a"
PROFILE="release"
OUT_DIR="target/ios"
FRAMEWORK="${OUT_DIR}/PimCore.xcframework"
HEADERS="crates/${PKG}/include"

DEVICE_TRIPLE="aarch64-apple-ios"
SIM_ARM_TRIPLE="aarch64-apple-ios-sim"
SIM_X86_TRIPLE="x86_64-apple-ios"

require_tool() {
  command -v "$1" >/dev/null 2>&1 \
    || { echo "error: required tool '$1' is not on PATH" >&2; exit 1; }
}

require_tool cargo
require_tool lipo
require_tool xcodebuild

# xcodebuild comes with Command Line Tools but `-create-xcframework`
# requires a full Xcode install. Fail fast with a pointer, not a cryptic
# xcode-select error 10 lines in.
DEV_DIR="$(xcode-select -p 2>/dev/null || true)"
if [[ "${DEV_DIR}" == "/Library/Developer/CommandLineTools" ]]; then
  echo "error: xcode-select currently points at Command Line Tools only." >&2
  echo "       xcodebuild -create-xcframework needs full Xcode." >&2
  echo "       Install Xcode from the App Store and run:" >&2
  echo "         sudo xcode-select -s /Applications/Xcode.app/Contents/Developer" >&2
  exit 1
fi

for triple in "$DEVICE_TRIPLE" "$SIM_ARM_TRIPLE" "$SIM_X86_TRIPLE"; do
  if ! rustup target list --installed | grep -q "^${triple}$"; then
    echo "error: rustup target ${triple} is not installed. Run:" >&2
    echo "  rustup target add ${triple}" >&2
    exit 1
  fi
done

echo "==> building ${PKG} for ${DEVICE_TRIPLE}"
cargo build --release -p "${PKG}" --target "${DEVICE_TRIPLE}"

echo "==> building ${PKG} for ${SIM_ARM_TRIPLE}"
cargo build --release -p "${PKG}" --target "${SIM_ARM_TRIPLE}"

echo "==> building ${PKG} for ${SIM_X86_TRIPLE}"
cargo build --release -p "${PKG}" --target "${SIM_X86_TRIPLE}"

SIM_FAT_DIR="${OUT_DIR}/sim-fat"
mkdir -p "${SIM_FAT_DIR}"
echo "==> fusing simulator arches with lipo -> ${SIM_FAT_DIR}/${LIB}"
lipo -create \
  "target/${SIM_ARM_TRIPLE}/${PROFILE}/${LIB}" \
  "target/${SIM_X86_TRIPLE}/${PROFILE}/${LIB}" \
  -output "${SIM_FAT_DIR}/${LIB}"

echo "==> assembling XCFramework -> ${FRAMEWORK}"
rm -rf "${FRAMEWORK}"
xcodebuild -create-xcframework \
  -library "target/${DEVICE_TRIPLE}/${PROFILE}/${LIB}" -headers "${HEADERS}" \
  -library "${SIM_FAT_DIR}/${LIB}"                    -headers "${HEADERS}" \
  -output "${FRAMEWORK}"

echo "==> done: ${FRAMEWORK}"
