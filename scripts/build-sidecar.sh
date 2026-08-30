#!/bin/sh
# Build the agent and the CLI and drop them where Tauri expects sidecars:
#   apps/desktop/src-tauri/binaries/filemind-agent-<host triple>
#   apps/desktop/src-tauri/binaries/filemind-<host triple>
# Tauri strips the triple when it copies them into the bundle
# (FileMind.app/Contents/MacOS/filemind-agent) and into target/ for `tauri dev`.
#
#   scripts/build-sidecar.sh --debug     # fast, for `npm run tauri dev`
#   scripts/build-sidecar.sh --release   # for `tauri build`
#   TARGET=x86_64-apple-darwin scripts/build-sidecar.sh --release   # cross target
set -eu

profile="release"
for a in "$@"; do
  case "$a" in
    --debug) profile="debug" ;;
    --release) profile="release" ;;
    *) echo "unknown flag $a" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/apps/desktop/src-tauri/binaries"
triple="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
mkdir -p "$out"

cd "$root"
if [ "$profile" = "release" ]; then
  cargo build --release -p filemind-agent -p filemind-cli ${TARGET:+--target "$TARGET"}
else
  cargo build -p filemind-agent -p filemind-cli ${TARGET:+--target "$TARGET"}
fi

src="$root/target${TARGET:+/$TARGET}/$profile"
ext=""
case "$triple" in *windows*) ext=".exe" ;; esac

cp -f "$src/filemind-agent$ext" "$out/filemind-agent-$triple$ext"
cp -f "$src/filemind$ext" "$out/filemind-$triple$ext"

# The sidecar must not depend on a dylib that is not in the bundle. ORT's
# prebuilt binaries are linked statically, so on macOS the only expected
# dependencies are system frameworks and /usr/lib. Fail loudly otherwise.
case "$triple" in
  *apple-darwin)
    if command -v otool >/dev/null 2>&1; then
      bad="$(otool -L "$out/filemind-agent-$triple" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/Library/)' || true)"
      if [ -n "$bad" ]; then
        echo "filemind-agent links libraries outside the bundle — add them to bundle.resources and sign them:" >&2
        echo "$bad" >&2
        exit 1
      fi
    fi
    ;;
esac

echo "sidecars ($profile, $triple):"
ls -la "$out" | grep -v '^total\|\.gitignore'
