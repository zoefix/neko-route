#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${VERSION:-$(node -e "console.log(require('./src-tauri/tauri.conf.json').version)")}"
TAG="${1:-${GITHUB_RELEASE_TAG:-v$VERSION}}"
BASE_URL="${GITHUB_RELEASE_BASE_URL:-https://github.com/zoefix/neko-route/releases/download/$TAG}"
OUT="${OUT:-release/latest.json}"
NOTES="${RELEASE_NOTES:-Neko Route $VERSION}"
PUB_DATE="${PUB_DATE:-$(date -u +"%Y-%m-%dT%H:%M:%SZ")}"
ALLOW_MISSING="${ALLOW_MISSING:-0}"
HOST_UNAME="$(uname -m 2>/dev/null || true)"

if [[ "$TAG" != "v$VERSION" && "${ALLOW_VERSION_MISMATCH:-0}" != "1" ]]; then
  echo "Release tag $TAG does not match app version $VERSION. Set ALLOW_VERSION_MISMATCH=1 to override." >&2
  exit 1
fi

find_first() {
  local pattern match
  for pattern in "$@"; do
    while IFS= read -r match; do
      if [[ -n "$match" && -f "$match" ]]; then
        printf '%s\n' "$match"
        return 0
      fi
    done < <(compgen -G "$pattern" || true)
  done
  return 1
}

read_sig() {
  local artifact="$1"
  local sig="${artifact}.sig"
  if [[ ! -f "$sig" ]]; then
    echo "Missing signature: $sig" >&2
    return 1
  fi
  tr -d '\r\n' < "$sig"
}

entries=()
missing=()

add_platform() {
  local key="$1"
  local alias="$2"
  shift 2
  local artifact
  if ! artifact="$(find_first "$@")"; then
    missing+=("$key")
    return 0
  fi
  local asset signature
  asset="$(basename "$artifact")"
  signature="$(read_sig "$artifact")"
  entries+=("$key|$alias|$asset|$signature")
}

add_platform "windows-x86_64-nsis" "windows-x86_64" \
  "release/windows-x64/*setup.exe" \
  "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*setup.exe"
add_platform "windows-aarch64-nsis" "windows-aarch64" \
  "release/windows-arm64/*setup.exe" \
  "src-tauri/target/aarch64-pc-windows-msvc/release/bundle/nsis/*setup.exe"
if [[ "$HOST_UNAME" == "arm64" || "$HOST_UNAME" == "aarch64" ]]; then
  add_platform "darwin-aarch64-app" "darwin-aarch64" \
    "release/macos-arm64/*.app.tar.gz" \
    "src-tauri/target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz" \
    "src-tauri/target/release/bundle/macos/*.app.tar.gz"
else
  add_platform "darwin-aarch64-app" "darwin-aarch64" \
    "release/macos-arm64/*.app.tar.gz" \
    "src-tauri/target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz"
fi
if [[ "$HOST_UNAME" == "x86_64" || "$HOST_UNAME" == "amd64" ]]; then
  add_platform "darwin-x86_64-app" "darwin-x86_64" \
    "release/macos-x64/*.app.tar.gz" \
    "src-tauri/target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz" \
    "src-tauri/target/release/bundle/macos/*.app.tar.gz"
else
  add_platform "darwin-x86_64-app" "darwin-x86_64" \
    "release/macos-x64/*.app.tar.gz" \
    "src-tauri/target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz"
fi
add_platform "linux-x86_64-deb" "linux-x86_64" \
  "release/linux-amd64/*.deb" \
  "src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/*.deb"
add_platform "linux-aarch64-deb" "linux-aarch64" \
  "release/linux-arm64/*.deb" \
  "src-tauri/target/aarch64-unknown-linux-gnu/release/bundle/deb/*.deb"

if (( ${#missing[@]} > 0 )) && [[ "$ALLOW_MISSING" != "1" ]]; then
  printf 'Missing updater artifacts for: %s\n' "${missing[*]}" >&2
  echo "Set ALLOW_MISSING=1 only for local manifest dry-runs." >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT")"

node --input-type=module - \
  "$VERSION" "$BASE_URL" "$NOTES" "$PUB_DATE" "$OUT" "${entries[@]}" <<'NODE'
import fs from "node:fs";

const [version, baseUrl, notes, pubDate, out, ...entries] = process.argv.slice(2);
const platforms = {};

function assetUrl(asset) {
  return `${baseUrl.replace(/\/$/, "")}/${encodeURIComponent(asset)}`;
}

for (const entry of entries) {
  const [key, alias, asset, signature] = entry.split("|");
  const value = { url: assetUrl(asset), signature };
  platforms[key] = value;
  if (alias && !platforms[alias]) {
    platforms[alias] = value;
  }
}

const manifest = {
  version,
  notes,
  pub_date: pubDate,
  platforms,
};

fs.writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`);
NODE

echo "Updater manifest: $OUT"

if [[ "${PUBLISH_RELEASE:-0}" == "1" ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "Missing required command: gh" >&2
    exit 1
  fi
  export GH_TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  if [[ -z "$GH_TOKEN" ]]; then
    echo "GH_TOKEN or GITHUB_TOKEN is required when PUBLISH_RELEASE=1" >&2
    exit 1
  fi

  mapfile -d '' assets < <(find release -mindepth 2 -type f ! -name ".DS_Store" -print0 | sort -z)
  if (( ${#assets[@]} == 0 )); then
    echo "No release assets found under release/*" >&2
    exit 1
  fi

  gh release view "$TAG" >/dev/null 2>&1 || gh release create "$TAG" \
    --title "Neko Route $TAG" \
    --notes "$NOTES"
  gh release upload "$TAG" "$OUT" "${assets[@]}" --clobber
  echo "Published GitHub release assets for $TAG"
fi
