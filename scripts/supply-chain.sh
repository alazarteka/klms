#!/usr/bin/env bash
set -euo pipefail

# Authenticate pinned release bytes before extracting or executing either tool.
command="${1:-}"
case "$command" in
  deny|vet) shift ;;
  *) echo "Usage: $0 {deny|vet} [check options]" >&2; exit 2 ;;
esac
host="$(uname -s)-$(uname -m)"
case "$command:$host" in
  deny:Darwin-arm64)
    target=aarch64-apple-darwin
    expected=fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3 ;;
  deny:Darwin-x86_64)
    target=x86_64-apple-darwin
    expected=248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6 ;;
  deny:Linux-aarch64)
    target=aarch64-unknown-linux-musl
    expected=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d ;;
  deny:Linux-x86_64)
    target=x86_64-unknown-linux-musl
    expected=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
  vet:Darwin-arm64)
    target=aarch64-apple-darwin
    expected=4b6cdbb7b1287428daf4a9bb3dc83b73ceb336893dbee6df35371a2e06ced51a ;;
  vet:Linux-x86_64)
    target=x86_64-unknown-linux-gnu
    expected=c7664d9db5dd2ff813f20303650ac8253fa712ff2a1ea9ce12bed71e346f1744 ;;
  *) echo "Unsupported platform for pinned cargo-$command: $host" >&2; exit 2 ;;
esac

case "$command" in
  deny)
    version=0.20.2
    root="cargo-deny-$version-$target"
    archive="$root.tar.gz"
    url="https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/$archive" ;;
  vet)
    version=0.10.0
    root="cargo-vet-$target"
    archive="$root.tar.xz"
    url="https://github.com/mozilla/cargo-vet/releases/download/v$version/$archive" ;;
esac

umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/klms-supply-chain.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
curl --proto '=https' --tlsv1.2 --fail --location --retry 3 \
  --output "$work_dir/$archive" "$url"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work_dir/$archive" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$work_dir/$archive" | awk '{print $1}')"
fi
if [[ "$actual" != "$expected" ]]; then
  echo "cargo-$command archive checksum mismatch" >&2
  echo "expected: $expected; actual: $actual" >&2
  exit 1
fi

# Stream only the authenticated executable member into a private ordinary file.
binary="$work_dir/cargo-$command"
tar -xOf "$work_dir/$archive" "$root/cargo-$command" > "$binary"
chmod 0700 "$binary"
case "$command" in
  deny) "$binary" --locked check "$@" ;;
  vet) CARGO="${CARGO:-$(command -v cargo)}" "$binary" vet check --locked "$@" ;;
esac
