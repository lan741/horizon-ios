#!/usr/bin/env bash
# Builds Horizon's iOS app and an IPA, the way .github/workflows/ios-ipa.yml does on a runner.
#
#   scripts/build-ios.sh              an unsigned IPA (sign it with Sideloadly, ESign, or a cert)
#   scripts/build-ios.sh --signed     an archive signed with a real identity in the keychain
#
# Needs macOS with Xcode 16+, Rust with the aarch64-apple-ios target, and xcodegen:
#   rustup target add aarch64-apple-ios
#   brew install xcodegen
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-unsigned}"
APP_VERSION="${APP_VERSION:-0.1.0}"
BUNDLE_ID="${BUNDLE_ID:-dev.horizon.ios}"
ARCHIVE_DIR="target/aarch64-apple-ios/release"

command -v xcodebuild >/dev/null || { echo "needs macOS and Xcode"; exit 1; }
command -v xcodegen >/dev/null || { echo "brew install xcodegen"; exit 1; }
rustup target list --installed | grep -qx aarch64-apple-ios || rustup target add aarch64-apple-ios

echo "==> cargo build (release, aarch64-apple-ios)"
# No default features reach horizon-client: horizon-ios asks for none, so the build has no
# controllers (gilrs has no iOS backend), no audio, and no Horizon extras.
cargo build -p horizon-ios --release --target aarch64-apple-ios

test -f "$ARCHIVE_DIR/libhorizon_ios.a" || { echo "missing $ARCHIVE_DIR/libhorizon_ios.a"; exit 1; }
mkdir -p ios/lib
cp -f "$ARCHIVE_DIR/libhorizon_ios.a" ios/lib/libhorizon_ios.a

echo "==> xcodegen"
(cd ios && xcodegen generate --spec project.yml)

echo "==> xcodebuild"
cd ios
rm -rf build Payload *.ipa
if [ "$MODE" = "--signed" ]; then
  xcodebuild -project Horizon.xcodeproj -scheme Horizon -configuration Release -sdk iphoneos \
    -archivePath build/Horizon.xcarchive archive -allowProvisioningUpdates \
    PRODUCT_BUNDLE_IDENTIFIER="$BUNDLE_ID" MARKETING_VERSION="$APP_VERSION" CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES \
    | tee archive.log | tail -n 60
  cat > ExportOptions.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>development</string>
  <key>signingStyle</key><string>automatic</string>
  <key>stripSwiftSymbols</key><true/>
  <key>compileBitcode</key><false/>
</dict>
</plist>
EOF
  xcodebuild -exportArchive -archivePath build/Horizon.xcarchive -exportPath build/export \
    -exportOptionsPlist ExportOptions.plist -allowProvisioningUpdates | tee export.log | tail -n 40
  cp build/export/*.ipa "Horizon-${APP_VERSION}-signed.ipa"
else
  xcodebuild -project Horizon.xcodeproj -scheme Horizon -configuration Release -sdk iphoneos \
    -derivedDataPath build \
    CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" \
    EXPANDED_CODE_SIGN_IDENTITY="" \
    PRODUCT_BUNDLE_IDENTIFIER="$BUNDLE_ID" MARKETING_VERSION="$APP_VERSION" \
    | tee build.log | tail -n 60
  APP_PATH="build/Build/Products/Release-iphoneos/Horizon.app"
  test -d "$APP_PATH" || { echo "missing $APP_PATH"; exit 1; }
  mkdir -p Payload
  cp -R "$APP_PATH" Payload/
  zip -qry "Horizon-${APP_VERSION}-unsigned.ipa" Payload
fi
ls -lh ./*.ipa
shasum -a 256 ./*.ipa