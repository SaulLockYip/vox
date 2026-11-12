#!/usr/bin/env bash
# 打包 Vox 为 macOS .app，ad-hoc 签名，并安装到 /Applications。
#
#   bash tools/package-macos.sh            # 打包 + 签名 + 安装
#   bash tools/package-macos.sh --no-install
#
# 说明：本机没有 Developer ID 证书，只能 ad-hoc 签名（codesign --sign -）。
# ad-hoc 签名的 app 本机可正常运行，但拷到别的 Mac 会被 Gatekeeper 拦。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Vox"
BUNDLE_ID="com.vox.desktop"
VERSION="0.1.1"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
INSTALL_DIR="/Applications"

export PATH="$HOME/.cargo/bin:$PATH"

echo "==> 编译 release"
cargo build --release -p vox-desktop

BIN="$ROOT/target/release/vox-desktop"
if [[ ! -x "$BIN" ]]; then
  echo "找不到可执行文件: $BIN" >&2
  exit 1
fi

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

cp "$ROOT/src-tauri/icons/icon.icns" "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Vox 需要使用麦克风来听写你的语音。</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Vox 需要与前台应用交互：读取你选中的文字，并把转写结果插入光标处。</string>
</dict>
</plist>
PLIST

echo "==> 签名（ad-hoc）"
codesign --force --sign - --timestamp=none "$APP"
codesign --verify --deep --strict "$APP" && echo "    签名校验通过"

if [[ "${1:-}" == "--no-install" ]]; then
  echo "==> 跳过安装：$APP"
  exit 0
fi

echo "==> 安装到 $INSTALL_DIR"
# 已经在跑就先退出，否则覆盖会失败
pkill -x "$APP_NAME" >/dev/null 2>&1 || true
sleep 0.5
rm -rf "$INSTALL_DIR/$APP_NAME.app"
cp -R "$APP" "$INSTALL_DIR/$APP_NAME.app"
codesign --verify --deep --strict "$INSTALL_DIR/$APP_NAME.app" && echo "    已安装并校验通过：$INSTALL_DIR/$APP_NAME.app"

echo "==> 完成。启动： open -a $APP_NAME"
