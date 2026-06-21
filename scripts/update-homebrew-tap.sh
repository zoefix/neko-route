#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TAG="${1:-${GITHUB_RELEASE_TAG:-}}"
if [[ -z "$TAG" ]]; then
  VERSION="$(node -e "console.log(require('./src-tauri/tauri.conf.json').version)")"
  TAG="v$VERSION"
else
  VERSION="${TAG#v}"
fi

if [[ "$TAG" != v* ]]; then
  TAG="v$TAG"
fi
VERSION="${TAG#v}"

TAP_REPO="${HOMEBREW_TAP_REPO:-zoefix/homebrew-neko-route}"
TAP_BRANCH="${HOMEBREW_TAP_BRANCH:-main}"
TAP_DIR="${HOMEBREW_TAP_DIR:-}"
BASE_URL="${GITHUB_RELEASE_BASE_URL:-https://github.com/zoefix/neko-route/releases/download/$TAG}"
ASSET_PREFIX="Neko-Route_${VERSION}_macos"
ARM_ASSET="${ASSET_PREFIX}_arm64.dmg"
X64_ASSET="${ASSET_PREFIX}_x64.dmg"

if [[ "${CI:-}" == "true" && -z "${HOMEBREW_TAP_TOKEN:-}" ]]; then
  echo "HOMEBREW_TAP_TOKEN is required to update $TAP_REPO from CI." >&2
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

sha256_for_asset() {
  local asset="$1"
  local path tmp
  if path="$(find_first "release/macos-arm64/$asset" "release/macos-x64/$asset" "release/**/$asset")"; then
    shasum -a 256 "$path" | awk '{print $1}'
    return 0
  fi

  tmp="$(mktemp)"
  curl -fsSL "$BASE_URL/$asset" -o "$tmp"
  shasum -a 256 "$tmp" | awk '{print $1}'
  rm -f "$tmp"
}

clone_tap() {
  local dir="$1"
  if [[ -n "${HOMEBREW_TAP_TOKEN:-}" ]]; then
    local auth
    auth="$(printf 'x-access-token:%s' "$HOMEBREW_TAP_TOKEN" | base64 | tr -d '\n')"
    git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth" \
      clone "https://github.com/$TAP_REPO.git" "$dir"
  else
    gh repo clone "$TAP_REPO" "$dir"
  fi
}

push_tap() {
  if [[ -n "${HOMEBREW_TAP_TOKEN:-}" ]]; then
    local auth
    auth="$(printf 'x-access-token:%s' "$HOMEBREW_TAP_TOKEN" | base64 | tr -d '\n')"
    git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth" \
      push origin "HEAD:$TAP_BRANCH"
  else
    git push origin "HEAD:$TAP_BRANCH"
  fi
}

cleanup_dir=""
if [[ -z "$TAP_DIR" ]]; then
  cleanup_dir="$(mktemp -d)"
  TAP_DIR="$cleanup_dir/homebrew-neko-route"
  clone_tap "$TAP_DIR"
fi
trap '[[ -n "$cleanup_dir" ]] && rm -rf "$cleanup_dir"' EXIT

ARM_SHA256="$(sha256_for_asset "$ARM_ASSET")"
X64_SHA256="$(sha256_for_asset "$X64_ASSET")"

mkdir -p "$TAP_DIR/Casks"
cd "$TAP_DIR"

if git rev-parse --verify HEAD >/dev/null 2>&1; then
  git checkout "$TAP_BRANCH" 2>/dev/null || git checkout -B "$TAP_BRANCH"
else
  git checkout -B "$TAP_BRANCH"
fi

cat > Casks/neko-route.rb <<CASK
cask "neko-route" do
  arch arm: "arm64", intel: "x64"

  version "$VERSION"
  sha256 arm:   "$ARM_SHA256",
         intel: "$X64_SHA256"

  url "https://github.com/zoefix/neko-route/releases/download/v#{version}/Neko-Route_#{version}_macos_#{arch}.dmg"
  name "Neko Route"
  desc "Local AI model router for Codex"
  homepage "https://github.com/zoefix/neko-route"

  livecheck do
    url "https://github.com/zoefix/neko-route/releases/latest"
    strategy :github_latest
  end

  auto_updates true
  depends_on :macos

  app "Neko Route.app"
end
CASK

cat > README.md <<README
# Homebrew Tap for Neko Route

Install Neko Route on macOS:

\`\`\`bash
brew install --cask zoefix/neko-route/neko-route
\`\`\`

Update Neko Route:

\`\`\`bash
brew update && brew upgrade --cask neko-route
\`\`\`

Neko Route releases are published from <https://github.com/zoefix/neko-route>.
README

git add Casks/neko-route.rb README.md
if git diff --cached --quiet; then
  echo "Homebrew tap is already up to date for $TAG."
  exit 0
fi

git -c user.name="${GIT_AUTHOR_NAME:-zoefix}" \
  -c user.email="${GIT_AUTHOR_EMAIL:-277453498+zoefix@users.noreply.github.com}" \
  commit -m "Update neko-route cask to $VERSION"
if [[ "${HOMEBREW_TAP_NO_PUSH:-0}" == "1" ]]; then
  echo "Prepared $TAP_REPO for $VERSION without pushing."
  exit 0
fi
push_tap
echo "Updated $TAP_REPO to $VERSION."
