#!/bin/sh
# Regenerate the Homebrew tap formula with the release's real sha256s and push.
# Args: <artifacts-dir> <tag> <repo>   (env: TAP_PAT)
set -eu

ARTIFACTS="$1"; TAG="$2"; REPO="$3"
OWNER="${REPO%/*}"
VERSION="${TAG#v}"
TAP_DIR="$(mktemp -d)"
trap 'rm -rf "$TAP_DIR"' EXIT

sha() { shasum -a 256 "$ARTIFACTS/torq-$1.tar.gz" | cut -d' ' -f1; }

git clone "https://x-access-token:${TAP_PAT}@github.com/${OWNER}/homebrew-torq.git" "$TAP_DIR"
mkdir -p "$TAP_DIR/Formula"
cat > "$TAP_DIR/Formula/torq.rb" <<EOF
class Torq < Formula
  desc "Fast torrent finder and downloader"
  homepage "https://github.com/${REPO}"
  version "${VERSION}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/${REPO}/releases/download/${TAG}/torq-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    else
      url "https://github.com/${REPO}/releases/download/${TAG}/torq-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/${REPO}/releases/download/${TAG}/torq-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    else
      url "https://github.com/${REPO}/releases/download/${TAG}/torq-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "torq"
  end

  test do
    assert_match "torq", shell_output("#{bin}/torq --version")
  end
end
EOF

cd "$TAP_DIR"
git add Formula/torq.rb
git -c user.name="torq-release" -c user.email="release@torq.invalid" \
    commit -m "torq ${VERSION}"
git push
echo "tap updated: https://github.com/${OWNER}/homebrew-torq"
