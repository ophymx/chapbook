#!/bin/sh -e
# Build the app as a hand-rolled .app — no Xcode project — against the
# simulator slice of the XCFramework, the way the demo is built. Three
# modules in order: the library, the app's model (the same sources
# `swift test` runs on macOS), then the screens, which only this script
# compiles because a `.app` is not something SwiftPM produces for iOS.
#
#   ./build.sh
#   xcrun simctl install booted build/ChapbookApp.app
#   xcrun simctl launch --console-pty booted com.ophymx.chapbook
cd "$(dirname "$0")"
SLICE=../Chapbook/Chapbook.xcframework/ios-arm64-simulator
[ -d "$SLICE" ] || { echo "run ../build-xcframework.sh first" >&2; exit 1; }
TARGET=arm64-apple-ios17.0-simulator

APP=build/ChapbookApp.app
rm -rf "$APP"
mkdir -p "$APP/fonts" "$APP/en.lproj"
cp ../../fixtures/fonts/CrimsonText-*.ttf "$APP/fonts/"
cp Info.plist "$APP/"
cp en.lproj/Localizable.strings "$APP/en.lproj/"

xcrun -sdk iphonesimulator swiftc \
    -target $TARGET -swift-version 6 -parse-as-library \
    -I "$SLICE/Headers" \
    -module-name Chapbook \
    -emit-module -emit-module-path build/Chapbook.swiftmodule \
    -emit-library -static -o build/libChapbook.a \
    ../Chapbook/Sources/Chapbook/*.swift

xcrun -sdk iphonesimulator swiftc \
    -target $TARGET -swift-version 6 -parse-as-library \
    -I build -I "$SLICE/Headers" \
    -module-name ChapbookAppModel \
    -emit-module -emit-module-path build/ChapbookAppModel.swiftmodule \
    -emit-library -static -o build/libChapbookAppModel.a \
    Sources/ChapbookAppModel/*.swift

xcrun -sdk iphonesimulator swiftc \
    -target $TARGET -swift-version 6 -parse-as-library \
    -I build -I "$SLICE/Headers" \
    Sources/ChapbookApp/*.swift \
    -L build -lChapbookAppModel -lChapbook -L "$SLICE" -lchapbook_ffi \
    -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __entitlements -Xlinker Entitlements.plist \
    -o "$APP/ChapbookApp"

# Ad hoc, like Xcode's own simulator builds — which carry their
# entitlements in the `__entitlements` section the link above creates,
# not in the signature; a signature that names them is refused at launch.
# The Keychain refuses a process with no application identifier outright
# (-34018), and a refused Keychain is a sign-in asked for on every launch.
codesign --force --sign - "$APP"

echo "built; install and launch it in a booted simulator"
