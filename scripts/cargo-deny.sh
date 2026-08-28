#!/usr/bin/env bash
set -euo pipefail

# cargo-deny is downloaded directly instead of through a third-party action so
# the executable is authenticated before it is extracted or run.
readonly CARGO_DENY_VERSION="0.20.2"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    readonly target="aarch64-apple-darwin"
    readonly expected_sha256="fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3"
    ;;
  Darwin-x86_64)
    readonly target="x86_64-apple-darwin"
    readonly expected_sha256="248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6"
    ;;
  Linux-aarch64)
    readonly target="aarch64-unknown-linux-musl"
    readonly expected_sha256="995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d"
    ;;
  Linux-x86_64)
    readonly target="x86_64-unknown-linux-musl"
    readonly expected_sha256="9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"
    ;;
  *)
    echo "Unsupported platform for the pinned cargo-deny binary: $(uname -s) $(uname -m)" >&2
    exit 2
    ;;
esac

readonly archive="cargo-deny-${CARGO_DENY_VERSION}-${target}.tar.gz"
readonly url="https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/${archive}"
readonly cache_root="${CARGO_DENY_CACHE_DIR:-${CARGO_TARGET_DIR:-target}/supply-chain-tools}"
readonly cached_archive="${cache_root}/${archive}"
readonly archive_root="cargo-deny-${CARGO_DENY_VERSION}-${target}"

umask 077
mkdir -p "$cache_root"
work_dir="$(mktemp -d "${cache_root}/cargo-deny-run.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
private_archive="${work_dir}/${archive}"

if [[ -f "$cached_archive" ]]; then
  # The shared cache is input only. Copy it into the private run directory so
  # the exact bytes hashed below are also the exact bytes later inspected.
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
  echo "cargo-deny archive checksum mismatch" >&2
  echo "expected: $expected_sha256" >&2
  echo "actual:   $actual_sha256" >&2
  exit 1
fi

# Only publish a verified archive to the shared cache. Extraction never reads
# from this path, avoiding a cache-replacement race between hashing and use.
if [[ ! -f "$cached_archive" ]]; then
  cache_candidate="${work_dir}/cache-candidate"
  cp "$private_archive" "$cache_candidate"
  chmod 0644 "$cache_candidate"
  mv "$cache_candidate" "$cached_archive"
fi

# The checksum authenticates the archive. The Python helper requires exact
# names and types, then stream-copies only the regular executable member into a
# newly created private ordinary file; it never asks tar to extract paths.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
downloaded_executable="${work_dir}/cargo-deny"
python3 "${script_dir}/safe_tool_archive.py" \
  "$private_archive" "$archive_root" cargo-deny "$downloaded_executable"

"$downloaded_executable" "$@"
