#!/bin/bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

profile="debug"
if [[ "${1:-}" == "--release" ]]; then
    profile="release"
fi

build_args=(--bins)
if [[ "$profile" == "release" ]]; then
    build_args+=(--release)
fi
# 开发机没有 Xcode Metal Toolchain 时，仅用于本地开发和权限验证。
if [[ "${FILEBACK_RUNTIME_SHADERS:-0}" == "1" ]]; then
    build_args+=(--features gpui/runtime_shaders)
fi

cargo build "${build_args[@]}"

app="$project_root/dist/FileBack.app"
contents="$app/Contents"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "$project_root/packaging/macos/Info.plist" "$contents/Info.plist"
cp "$project_root/packaging/macos/dev.fileback.FileBack.helper.plist" "$contents/Resources/dev.fileback.FileBack.helper.plist"
cp "$project_root/target/$profile/fileback" "$contents/MacOS/fileback"
cp "$project_root/target/$profile/fileback-cli" "$contents/MacOS/fileback-cli"
plutil -lint "$contents/Info.plist" "$contents/Resources/dev.fileback.FileBack.helper.plist"

# 本地开发使用 ad-hoc 签名；发布版本应替换为 Developer ID 并公证。
codesign --force --sign - --identifier dev.fileback.FileBack.helper "$contents/MacOS/fileback-cli"
codesign --force --sign - --identifier dev.fileback.FileBack "$app"
codesign --verify --deep --strict --verbose=2 "$app"

printf '已生成：%s\n' "$app"
