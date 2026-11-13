#!/bin/bash
# Builds "Fruity Bridge.app" with swiftc (Command Line Tools are enough), signs it and runs
# the self-tests. Output: mac/build/Fruity Bridge.app (or the directory given as $1).
# UNIVERSAL=1 also builds x86_64 and merges with lipo.
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then
  echo "Build Fruity Bridge on macOS (Apple's Command Line Tools are enough)." >&2
  exit 1
fi
source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
out_dir="${1:-$source_dir/build}"
mkdir -p "$out_dir"
out_dir="$(cd -- "$out_dir" && pwd)"
version="${FRUITY_BRIDGE_VERSION:-0.1.0}"
app="$out_dir/Fruity Bridge.app"
work="$(mktemp -d "${TMPDIR:-/tmp}/fruity-bridge-build.XXXXXX")"
trap 'rm -rf "$work"' EXIT

archs=(arm64)
if [[ "${UNIVERSAL:-0}" == 1 ]]; then archs+=(x86_64); fi
for arch in "${archs[@]}"; do
  xcrun swiftc -parse-as-library -O -swift-version 5 -target "$arch-apple-macosx14.0" \
    -framework AppKit -framework EventKit -framework CoreServices \
    -framework UniformTypeIdentifiers -lsqlite3 -lcompression \
    "$source_dir"/Sources/*.swift -o "$work/FruityBridge-$arch"
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if [[ ${#archs[@]} -gt 1 ]]; then
  lipo -create "$work"/FruityBridge-* -output "$app/Contents/MacOS/FruityBridge"
else
  cp "$work/FruityBridge-arm64" "$app/Contents/MacOS/FruityBridge"
fi
sed "s/__VERSION__/$version/g" "$source_dir/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
# A stable signing identity keeps macOS permissions (Full Disk Access, Reminders, Automation)
# across rebuilds; an ad-hoc signature changes with every build and macOS forgets them.
# The identity lives in its own keychain, created once by make-signing-identity.sh.
signing_dir="$HOME/Library/Application Support/Fruity Bridge/signing"
signing_keychain="$signing_dir/signing.keychain-db"
identity="${FRUITY_BRIDGE_SIGNING_IDENTITY:-Fruity Bridge Local Signing}"
if [[ -f "$signing_keychain" && -f "$signing_dir/keychain-password" ]]; then
  security unlock-keychain -p "$(cat "$signing_dir/keychain-password")" "$signing_keychain"
  hash="$(security find-certificate -c "$identity" -Z "$signing_keychain" | awk '/SHA-1/ {print $3}')"
  codesign --force --sign "$hash" --identifier com.plgeek.fruitybridge "$app"
else
  echo "No signing keychain; using an ad-hoc signature (permissions reset on every build)." >&2
  codesign --force --sign - --identifier com.plgeek.fruitybridge "$app"
fi
codesign --verify --strict "$app"
"$app/Contents/MacOS/FruityBridge" --selftest
echo "Built $app"
