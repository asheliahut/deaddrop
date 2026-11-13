#!/usr/bin/env bash
# Render the Homebrew formula for a release from its SHA256SUMS file.
#
#   packaging/homebrew/render-formula.sh v2.0.0 dist/SHA256SUMS > deaddrop.rb
#
# Linux uses the static musl builds so the formula works on any distro.
set -euo pipefail

tag="${1:?usage: render-formula.sh <tag> <SHA256SUMS>}"
sums="${2:?usage: render-formula.sh <tag> <SHA256SUMS>}"
repo="${GITHUB_REPOSITORY:-asheliahut/deaddrop}"
base="https://github.com/$repo/releases/download/$tag"

sha() {
  local file="deaddrop-$tag-$1.tar.gz"
  local hash
  hash="$(awk -v f="$file" '$2 == f || $2 == "*"f { print $1 }' "$sums")"
  if [ -z "$hash" ]; then
    echo "render-formula: no checksum for $file in $sums" >&2
    exit 1
  fi
  echo "$hash"
}

cat <<EOF
class Deaddrop < Formula
  desc "Leech a torrent without seeding and hand it off peer-to-peer over iroh"
  homepage "https://deaddrop.rip"
  license "Unlicense"

  on_macos do
    on_arm do
      url "$base/deaddrop-$tag-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$base/deaddrop-$tag-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$base/deaddrop-$tag-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-musl)"
    end
    on_intel do
      url "$base/deaddrop-$tag-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-musl)"
    end
  end

  def install
    bin.install "deaddrop"
    pkgshare.install "config.example.toml"
    generate_completions_from_executable(bin/"deaddrop", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/deaddrop --version")
  end
end
EOF
