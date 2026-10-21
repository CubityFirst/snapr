#!/usr/bin/env bash
# Packs a macOS build of snapr as snapr.app, then as the release's two Mac
# files: a .dmg to install from, and a zip of the app for the updater
# (src/update.rs), which swaps the whole app. Runs on macOS.
#
#   scripts/macos-bundle.sh <snapr binary> <version> <arch> <out dir>
#
# Writes <out dir>/snapr-macos-<arch>.dmg and snapr-macos-<arch>.app.zip.

set -euo pipefail

if [ $# -ne 4 ]; then
  echo "usage: $0 <snapr binary> <version> <arch> <out dir>" >&2
  exit 2
fi
bin=$1 version=$2 arch=$3 out=$4

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/snapr.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/snapr"
chmod 755 "$app/Contents/MacOS/snapr"
# The app icon, drawn by snapr's own icon code (`cargo test write_iconset
# -- --ignored` writes the iconset).
iconutil -c icns "$(dirname "$0")/../assets/snapr.iconset" -o "$app/Contents/Resources/snapr.icns"

# The bundle identifier is part of what macOS files granted permissions
# under (with the signature, below): keep it. LSUIElement keeps snapr out of the Dock from launch (it lives in the
# menu bar). The usage descriptions are what macOS shows when asking for
# the microphone and for system audio; without them it refuses outright.
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>st.cubityfir.snapr</string>
	<key>CFBundleName</key>
	<string>snapr</string>
	<key>CFBundleDisplayName</key>
	<string>snapr</string>
	<key>CFBundleExecutable</key>
	<string>snapr</string>
	<key>CFBundleIconFile</key>
	<string>snapr</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>snapr records your microphone with the screen when you turn that on in Settings.</string>
	<key>NSAudioCaptureUsageDescription</key>
	<string>snapr records what's playing with the screen when you turn that on in Settings.</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist"

# macOS files granted permissions under the signature's designated
# requirement as well as the bundle identifier. An ad hoc signature's
# requirement is the hash of this very build, so every update would lose
# them; signing with the same certificate every time (the release workflow's
# self-signed one, named in SNAPR_SIGN_IDENTITY) makes it name the
# certificate instead. Without one it falls back to ad hoc, which still seals
# the app (Apple silicon won't run unsigned code).
identity=${SNAPR_SIGN_IDENTITY:--}
codesign --force --sign "$identity" ${SNAPR_SIGN_KEYCHAIN:+--keychain "$SNAPR_SIGN_KEYCHAIN"} "$app"
codesign --verify --strict --verbose=2 "$app"
requirement=$(codesign --display --requirements - "$app" 2>&1)
echo "$requirement"
if [ "$identity" != - ] && grep -q cdhash <<<"$requirement"; then
  echo "error: the designated requirement is the build's hash, not the certificate" >&2
  exit 1
fi

mkdir -p "$out"
ditto -c -k --keepParent "$app" "$out/snapr-macos-$arch.app.zip"

# The disk image opens to the app beside a link to Applications to drag it
# onto.
stage="$work/dmg"
mkdir "$stage"
ditto "$app" "$stage/snapr.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname snapr -srcfolder "$stage" -fs HFS+ -format UDZO -ov \
  "$out/snapr-macos-$arch.dmg"
