#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
mkdir -p dist
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"

HOST="$(rustc -vV | sed -n 's/^host: //p')"

build_target() {
  local target="$1" out="$2"
  echo "==> cargo build --release --target ${target}"
  cargo build --release --target "${target}"
  cp "target/${target}/release/ami-s3-archive${3:-}" "dist/${out}"
}

echo "==> release build for host (${HOST})"
cargo build --release
cp "target/release/ami-s3-archive" "dist/ami-s3-archive-${HOST}"

if command -v rustup >/dev/null 2>&1; then
  rustup target add "${HOST}" 2>/dev/null || true
fi

if [[ "${HOST}" == "aarch64-apple-darwin" || "${HOST}" == "x86_64-apple-darwin" ]]; then
  cp "target/release/ami-s3-archive" "dist/ami-s3-archive-darwin-$(uname -m)"
fi

if command -v rustup >/dev/null 2>&1; then
  rustup target add x86_64-unknown-linux-musl 2>/dev/null || true
fi

if command -v x86_64-linux-musl-gcc >/dev/null 2>&1 || command -v musl-gcc >/dev/null 2>&1; then
  export CC_x86_64_unknown_linux_musl="${CC_x86_64_unknown_linux_musl:-x86_64-linux-musl-gcc}"
  build_target x86_64-unknown-linux-musl ami-s3-archive-linux-amd64
else
  echo "Skipping linux/musl build: install musl-cross (e.g. brew install messense/macos-cross-toolchains/x86_64-unknown-linux-musl)" >&2
fi

if command -v rustup >/dev/null 2>&1; then
  rustup target add x86_64-pc-windows-gnu 2>/dev/null || true
fi

if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  build_target x86_64-pc-windows-gnu ami-s3-archive-windows-amd64.exe .exe
else
  echo "Skipping Windows GNU build: x86_64-w64-mingw32-gcc not found" >&2
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum dist/* > dist/SHA256SUMS.txt
else
  shasum -a 256 dist/* > dist/SHA256SUMS.txt
fi

echo "==> dist/"
ls -lh dist/
