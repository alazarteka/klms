#!/usr/bin/env bash
set -euo pipefail

# cargo-vet is downloaded directly from its official, immutable release. The
# archive's digest was independently checked against Mozilla's published
# v0.10.0 .sha256 asset before being committed here.
readonly CARGO_VET_VERSION="0.10.0"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    readonly target="aarch64-apple-darwin"
    readonly expected_sha256="4b6cdbb7b1287428daf4a9bb3dc83b73ceb336893dbee6df35371a2e06ced51a"
    ;;
  Linux-x86_64)
    readonly target="x86_64-unknown-linux-gnu"
    readonly expected_sha256="c7664d9db5dd2ff813f20303650ac8253fa712ff2a1ea9ce12bed71e346f1744"
    ;;
  *)
    echo "Unsupported platform for the pinned cargo-vet binary: $(uname -s) $(uname -m)" >&2
    exit 2
    ;;
esac

readonly archive="cargo-vet-${target}.tar.xz"
readonly url="https://github.com/mozilla/cargo-vet/releases/download/v${CARGO_VET_VERSION}/${archive}"
readonly cache_root="${CARGO_VET_CACHE_DIR:-${CARGO_TARGET_DIR:-target}/supply-chain-tools}"
readonly cached_archive="${cache_root}/cargo-vet-${CARGO_VET_VERSION}-${target}.tar.xz"
readonly archive_root="cargo-vet-${target}"

umask 077
mkdir -p "$cache_root"
work_dir="$(mktemp -d "${cache_root}/cargo-vet-run.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
private_archive="${work_dir}/${archive}"

if [[ -f "$cached_archive" ]]; then
  cp "$cached_archive" "$private_archive"
else
  curl --proto '=https' --tlsv1.2 --fail --location --retry 3 \
    --output "$private_archive" "$url"
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual_sha256="$(sha256sum "$private_archive" | awk '{print $1}')"
else
  actual_sha256="$(shasum -a 256 "$private_archive" | awk '{print $1}')"
fi
if [[ "$actual_sha256" != "$expected_sha256" ]]; then
  echo "cargo-vet archive checksum mismatch" >&2
  echo "expected: $expected_sha256" >&2
  echo "actual:   $actual_sha256" >&2
  exit 1
fi

if [[ ! -f "$cached_archive" ]]; then
  cache_candidate="${work_dir}/cache-candidate"
  cp "$private_archive" "$cache_candidate"
  chmod 0644 "$cache_candidate"
  mv "$cache_candidate" "$cached_archive"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
downloaded_executable="${work_dir}/cargo-vet"
python3 "${script_dir}/safe_tool_archive.py" \
  "$private_archive" "$archive_root" cargo-vet "$downloaded_executable"

readonly vet_cache="${CARGO_VET_STATE_CACHE_DIR:-${cache_root}/cargo-vet-cache}"
mkdir -p "$vet_cache"
CARGO="${CARGO:-$(command -v cargo)}" "$downloaded_executable" vet "$@" --cache-dir "$vet_cache"
