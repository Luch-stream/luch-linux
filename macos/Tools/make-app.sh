#!/bin/bash
# Builds Luch.app from the SwiftPM executable and signs it. Stable signing matters: Screen Recording
# permission is tied to the signature, and ad-hoc builds need a fresh grant after every rebuild.
#
#   ./Tools/make-app.sh                     # ad-hoc
#   SIGN_ID="<your code-signing identity>" ./Tools/make-app.sh
#   REPORT_DSN="https://<key>@<host>/<project>" ./Tools/make-app.sh   # "Send to the Developers" (GlitchTip)
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

APP=".build/Luch.app"
BIN=".build/release/LuchApp"

if pgrep -f "Luch.app/Contents/MacOS/LuchApp" >/dev/null 2>&1; then
    echo "Luch.app is running — quit it from the menu first"
    exit 1
fi

export CLANG_MODULE_CACHE_PATH="$PWD/.mc" SWIFT_MODULECACHE_PATH="$PWD/.mc"
swift build --disable-sandbox -c release --cache-path ./.spm-cache --scratch-path ./.build

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$BIN" "$APP/Contents/MacOS/LuchApp"
# The command line and the browser extension's host, at a path that stays when the app is moved into /Applications.
cp ".build/release/luchctl" "$APP/Contents/MacOS/luchctl"
# Loaded by the system perl to read what plays (Sources/LuchEngine/NowPlaying.swift).
cp ".build/release/libLuchNowPlaying.dylib" "$APP/Contents/Frameworks/"
cp ../assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
# libopus goes inside the app: linked from Homebrew, the app would not start on a Mac without `brew install opus`.
OPUS=$(otool -L "$APP/Contents/MacOS/LuchApp" | awk '/libopus/ {print $1}')
if [ -n "$OPUS" ]; then
    cp -L "$OPUS" "$APP/Contents/Frameworks/libopus.0.dylib"
    chmod u+w "$APP/Contents/Frameworks/libopus.0.dylib"
    install_name_tool -id @rpath/libopus.0.dylib "$APP/Contents/Frameworks/libopus.0.dylib"
    install_name_tool -change "$OPUS" @executable_path/../Frameworks/libopus.0.dylib "$APP/Contents/MacOS/LuchApp"
fi

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>LuchApp</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleIdentifier</key><string>dev.luch.mac</string>
    <key>CFBundleName</key><string>Luch</string>
    <key>CFBundleDisplayName</key><string>Luch</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>Luch finds receivers on the local network with Bonjour and streams the screen to them.</string>
    <key>NSBluetoothAlwaysUsageDescription</key>
    <string>Luch finds receivers nearby over Bluetooth, and learns your TV remote's power key to turn the TV on.</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>Luch listens for a TV saying its address by sound, or times the TVs' sound to play a group together, only while you ask it to.</string>
    <key>NSCameraUsageDescription</key>
    <string>Luch reads the QR code on a TV's screen, only while you ask it to find receivers that way.</string>
    <key>NSLocationUsageDescription</key>
    <string>Luch reads the name of your Wi-Fi network only when you connect directly to a TV, to come back to it after.</string>
    <key>NSLocationWhenInUseUsageDescription</key>
    <string>Luch reads the name of your Wi-Fi network only when you connect directly to a TV, to come back to it after.</string>
    <key>NSBonjourServices</key>
    <array>
        <string>_luch._tcp</string>
        <string>_luchsrc._tcp</string>
    </array>
    <!-- Finder's Quick Actions / Services for selected files (Sources/LuchApp/ShowFiles.swift). -->
    <key>NSServices</key>
    <array>
        <dict>
            <key>NSMenuItem</key><dict><key>default</key><string>Show on TV with Luch</string></dict>
            <key>NSMessage</key><string>showOnTV</string>
            <key>NSPortName</key><string>Luch</string>
            <key>NSRequiredContext</key><dict/>
            <key>NSSendFileTypes</key>
            <array>
                <string>public.image</string>
                <string>public.movie</string>
                <string>public.audio</string>
                <string>com.adobe.pdf</string>
                <string>public.presentation</string>
                <string>com.apple.keynote.key</string>
                <string>org.openxmlformats.presentationml.presentation</string>
                <string>com.microsoft.powerpoint.ppt</string>
                <string>org.oasis-open.opendocument.presentation</string>
                <string>org.openxmlformats.wordprocessingml.document</string>
                <string>com.microsoft.word.doc</string>
                <string>org.openxmlformats.spreadsheetml.sheet</string>
                <string>com.microsoft.excel.xls</string>
                <string>org.oasis-open.opendocument.text</string>
                <string>org.oasis-open.opendocument.spreadsheet</string>
                <string>public.rtf</string>
            </array>
        </dict>
        <dict>
            <key>NSMenuItem</key><dict><key>default</key><string>Present on TV with Luch</string></dict>
            <key>NSMessage</key><string>presentOnTV</string>
            <key>NSPortName</key><string>Luch</string>
            <key>NSRequiredContext</key><dict/>
            <key>NSSendFileTypes</key>
            <array>
                <string>com.adobe.pdf</string>
                <string>public.presentation</string>
                <string>com.apple.keynote.key</string>
                <string>org.openxmlformats.presentationml.presentation</string>
                <string>com.microsoft.powerpoint.ppt</string>
                <string>org.oasis-open.opendocument.presentation</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST
# The report address (LuchReport/ReportSender.swift): without one the app has no "Send to the Developers".
if [ -n "${REPORT_DSN:-}" ]; then
    /usr/libexec/PlistBuddy -c "Add :LuchReportDSN string $REPORT_DSN" "$APP/Contents/Info.plist"
fi

SIGN_ID="${SIGN_ID:-}"
LIBS="$APP/Contents/Frameworks/libLuchNowPlaying.dylib"
[ -f "$APP/Contents/Frameworks/libopus.0.dylib" ] && LIBS="$LIBS $APP/Contents/Frameworks/libopus.0.dylib"
# A Developer ID build is for others: the hardened runtime and a timestamp, which notarization asks for, and the
# devices the runtime then needs named (the microphone for calibration and finding by sound, the camera for QR codes).
OPTS=""
case "$SIGN_ID" in
    "Developer ID"*|[0-9A-F]*) if security find-identity -v -p codesigning | grep -F "$SIGN_ID" | grep -q "Developer ID"; then
        OPTS="--options runtime --timestamp --entitlements Tools/Luch.entitlements"; fi ;;
esac
if [ -n "$SIGN_ID" ]; then
    # shellcheck disable=SC2086
    codesign --force $OPTS --sign "$SIGN_ID" $LIBS "$APP/Contents/MacOS/luchctl"
    # shellcheck disable=SC2086
    if ! codesign --force $OPTS --sign "$SIGN_ID" "$APP"; then
        echo "codesign failed with: $SIGN_ID" >&2
        exit 1
    fi
    echo "signed with: $SIGN_ID"
else
    # shellcheck disable=SC2086
    codesign --force --sign - $LIBS "$APP/Contents/MacOS/luchctl"
    codesign --force --sign - "$APP"
    echo "signed ad-hoc (Screen Recording is asked again after every rebuild; pass SIGN_ID for a stable identity)"
fi
echo "built $APP"
echo "run: open $APP"
