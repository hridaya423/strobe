#!/bin/sh
set -eu
cd "$(dirname "$0")"
CEF_PATH="${CEF_PATH:-$HOME/.local/share/cef}"
export CEF_PATH
cargo build --release --bin harness >&2
APP=target/bundle/harness.app
FRAMEWORK="Chromium Embedded Framework.framework"
plist() {
  cat > "$1/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>$2</string>
<key>CFBundleIdentifier</key><string>$3</string>
<key>CFBundleName</key><string>$2</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1.0</string>
<key>LSUIElement</key><string>1</string>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict></plist>
PLIST
}
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks"
rm -f "$APP/Contents/MacOS/harness"
cp target/release/harness "$APP/Contents/MacOS/harness"
plist "$APP" harness com.vrf.spike.harness
if [ ! -d "$APP/Contents/Frameworks/$FRAMEWORK" ]; then
  cp -c -R "$CEF_PATH/$FRAMEWORK" "$APP/Contents/Frameworks/"
fi
for kind in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
  name="harness Helper$kind"
  dir="$APP/Contents/Frameworks/$name.app"
  mkdir -p "$dir/Contents/MacOS"
  rm -f "$dir/Contents/MacOS/$name"
  cp target/release/harness "$dir/Contents/MacOS/$name"
  id=$(printf '%s' "$kind" | tr -d ' ()' | tr 'A-Z' 'a-z')
  plist "$dir" "$name" "com.vrf.spike.harness.helper${id:+.$id}"
done
exec "$APP/Contents/MacOS/harness" "$@"
