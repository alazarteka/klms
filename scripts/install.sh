#!/usr/bin/env bash
set -euo pipefail

die() {
  echo "klms installer: $*" >&2
  exit 1
}

for tool in curl install mktemp tar uname; do
  command -v "$tool" >/dev/null 2>&1 || die "required command not found: $tool"
done

readonly repo="alazarteka/klms"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) readonly target="aarch64-apple-darwin" ;;
  Linux-x86_64) readonly target="x86_64-unknown-linux-musl" ;;
  *) die "no prebuilt release for $(uname -s) $(uname -m)" ;;
esac

latest_url="$(curl --proto '=https' --tlsv1.2 -fsSLI \
  -o /dev/null -w '%{url_effective}' \
  "https://github.com/${repo}/releases/latest")"
version="${latest_url%/}"
version="${version##*/}"
case "$version" in
  v[0-9]*) ;;
  *) die "could not determine the latest release version" ;;
esac

readonly archive="klms-${version}-${target}.tar.gz"
readonly download="https://github.com/${repo}/releases/download/${version}"
work_dir="$(mktemp -d)"
readonly work_dir
trap 'rm -rf "$work_dir"' EXIT

curl --proto '=https' --tlsv1.2 -fsSL \
  -o "$work_dir/$archive" "$download/$archive"
curl --proto '=https' --tlsv1.2 -fsSL \
  -o "$work_dir/$archive.sha256" "$download/$archive.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$work_dir" && sha256sum -c "$archive.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$work_dir" && shasum -a 256 -c "$archive.sha256")
else
  die "sha256sum or shasum is required to verify the download"
fi

tar -xzf "$work_dir/$archive" -C "$work_dir"
readonly install_dir="${KLMS_INSTALL_DIR:-$HOME/.local/bin}"
install -d "$install_dir"
install -m 0755 "$work_dir/klms-${version}-${target}/klms" "$install_dir/klms"
"$install_dir/klms" skill install

echo "Installed $("$install_dir/klms" --version) at $install_dir/klms"
