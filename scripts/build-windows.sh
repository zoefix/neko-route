#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ARCH="${1:-x64}"
case "$ARCH" in
  x64|x86_64|amd64)
    TARGET="x86_64-pc-windows-msvc"
    XWIN_ARCH="x86_64"
    RELEASE_DIR="release/windows-x64"
    ;;
  arm64|aarch64)
    TARGET="aarch64-pc-windows-msvc"
    XWIN_ARCH="aarch64"
    RELEASE_DIR="release/windows-arm64"
    ;;
  *)
    echo "Usage: $0 [x64|arm64]" >&2
    exit 2
    ;;
esac

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

need cargo
need corepack
need makensis

XWIN="${XWIN_ROOT:-$HOME/Library/Caches/cargo-xwin/xwin}"
if [[ ! -d "$XWIN/crt/lib/$XWIN_ARCH" || ! -d "$XWIN/sdk/lib/um/$XWIN_ARCH" || ! -d "$XWIN/sdk/lib/ucrt/$XWIN_ARCH" ]]; then
  if command -v cargo-xwin >/dev/null 2>&1; then
    cargo xwin env --target "$TARGET" >/dev/null
  fi
fi

for path in \
  "$XWIN/crt/include" \
  "$XWIN/sdk/include/ucrt" \
  "$XWIN/sdk/include/um" \
  "$XWIN/sdk/include/shared" \
  "$XWIN/sdk/include/winrt" \
  "$XWIN/crt/lib/$XWIN_ARCH" \
  "$XWIN/sdk/lib/um/$XWIN_ARCH" \
  "$XWIN/sdk/lib/ucrt/$XWIN_ARCH"; do
  if [[ ! -e "$path" ]]; then
    echo "Missing xwin SDK path: $path" >&2
    exit 1
  fi
done

if command -v brew >/dev/null 2>&1; then
  LLVM_PREFIX="${LLVM_PREFIX:-$(brew --prefix llvm@21 2>/dev/null || brew --prefix llvm 2>/dev/null || true)}"
  LLD_PREFIX="${LLD_PREFIX:-$(brew --prefix lld@21 2>/dev/null || brew --prefix lld 2>/dev/null || true)}"
else
  LLVM_PREFIX="${LLVM_PREFIX:-}"
  LLD_PREFIX="${LLD_PREFIX:-}"
fi

if [[ -n "$LLVM_PREFIX" ]]; then
  export PATH="$LLVM_PREFIX/bin:$PATH"
fi
if [[ -n "$LLD_PREFIX" ]]; then
  export PATH="$LLD_PREFIX/bin:$PATH"
fi

need clang
need clang++
need llvm-lib
need llvm-rc
need lld-link

export CC_aarch64_pc_windows_msvc=clang
export CXX_aarch64_pc_windows_msvc=clang++
export AR_aarch64_pc_windows_msvc=llvm-lib
export RC_aarch64_pc_windows_msvc=llvm-rc
export CC_x86_64_pc_windows_msvc=clang
export CXX_x86_64_pc_windows_msvc=clang++
export AR_x86_64_pc_windows_msvc=llvm-lib
export RC_x86_64_pc_windows_msvc=llvm-rc
export TARGET_CC=clang
export TARGET_CXX=clang++
export TARGET_AR=llvm-lib

export CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER=lld-link
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=lld-link
export CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUSTFLAGS="-C linker-flavor=lld-link -Lnative=$XWIN/crt/lib/aarch64 -Lnative=$XWIN/sdk/lib/um/aarch64 -Lnative=$XWIN/sdk/lib/ucrt/aarch64"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C linker-flavor=lld-link -Lnative=$XWIN/crt/lib/x86_64 -Lnative=$XWIN/sdk/lib/um/x86_64 -Lnative=$XWIN/sdk/lib/ucrt/x86_64"

COMMON_CFLAGS="--target=$TARGET -isystem $XWIN/crt/include -isystem $XWIN/sdk/include/ucrt -isystem $XWIN/sdk/include/um -isystem $XWIN/sdk/include/shared -isystem $XWIN/sdk/include/winrt"
export CFLAGS_aarch64_pc_windows_msvc="$COMMON_CFLAGS"
export CXXFLAGS_aarch64_pc_windows_msvc="$COMMON_CFLAGS"
export CFLAGS_x86_64_pc_windows_msvc="$COMMON_CFLAGS"
export CXXFLAGS_x86_64_pc_windows_msvc="$COMMON_CFLAGS"
export RCFLAGS="-I$XWIN/crt/include -I$XWIN/sdk/include/ucrt -I$XWIN/sdk/include/um -I$XWIN/sdk/include/shared -I$XWIN/sdk/include/winrt"
export CMAKE_SYSTEM_NAME=Windows
export CMAKE_GENERATOR=Ninja

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" && -f "$ROOT/updater-signing-key.local" ]]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(<"$ROOT/updater-signing-key.local")"
fi

corepack pnpm tauri build --target "$TARGET" --bundles nsis --ci

ARTIFACT_DIR="src-tauri/target/$TARGET/release/bundle/nsis"
ARTIFACT="$(find "$ARTIFACT_DIR" -maxdepth 1 -type f -name '*setup.exe' -print -quit)"
if [[ -z "$ARTIFACT" ]]; then
  echo "No NSIS installer found in $ARTIFACT_DIR" >&2
  exit 1
fi

mkdir -p "$RELEASE_DIR"
DEST="$RELEASE_DIR/$(basename "$ARTIFACT")"
cp "$ARTIFACT" "$DEST"

echo "Installer: $DEST"
UPDATER_ARTIFACT="$(find "$ARTIFACT_DIR" -maxdepth 1 -type f -name '*setup.exe.zip' -print -quit)"
if [[ -n "$UPDATER_ARTIFACT" ]]; then
  UPDATER_DEST="$RELEASE_DIR/$(basename "$UPDATER_ARTIFACT")"
  cp "$UPDATER_ARTIFACT" "$UPDATER_DEST"
  cp "${UPDATER_ARTIFACT}.sig" "${UPDATER_DEST}.sig"
  echo "Updater: $UPDATER_DEST"
fi
if command -v file >/dev/null 2>&1; then
  file "src-tauri/target/$TARGET/release/neko-route.exe" "$DEST"
fi
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$DEST"
fi
