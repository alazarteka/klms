#!/usr/bin/env bash
set -euo pipefail

die() {
  echo "klms installer: $*" >&2
  exit 1
}

for tool in chmod curl mkdir mktemp tar uname; do
  command -v "$tool" >/dev/null 2>&1 || die "required command not found: $tool"
done

readonly repo="alazarteka/klms"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) readonly target="aarch64-apple-darwin" ;;
  Linux-x86_64) readonly target="x86_64-unknown-linux-musl" ;;
  *) die "no prebuilt release for $(uname -s) $(uname -m)" ;;
esac

latest_url="$(curl --proto '=https' --tlsv1.2 --max-time 300 --max-redirs 5 -fsSLI \
  -o /dev/null -w '%{url_effective}' \
  "https://github.com/${repo}/releases/latest")"
version="${latest_url%/}"
version="${version##*/}"
[[ "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || die "latest release is not a stable version"

readonly archive="klms-${version}-${target}.tar.gz"
readonly download="https://github.com/${repo}/releases/download/${version}"
work_dir="$(mktemp -d)"
readonly work_dir
trap 'rm -rf "$work_dir"' EXIT

curl --proto '=https' --tlsv1.2 --max-time 300 --max-redirs 5 --max-filesize 67108864 -fsSL \
  -o "$work_dir/$archive" "$download/$archive"
curl --proto '=https' --tlsv1.2 --max-time 300 --max-redirs 5 --max-filesize 67108864 -fsSL \
  -o "$work_dir/$archive.sha256" "$download/$archive.sha256"

# Compare the digest ourselves: a checksum file cannot select other paths.
read -r expected checksum_name extra < "$work_dir/$archive.sha256" || die "invalid checksum file"
[[ "$expected" =~ ^[0-9a-f]{64}$ && "${checksum_name#\*}" == "$archive" && -z "${extra:-}" ]] || die "invalid checksum file"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work_dir/$archive")"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$work_dir/$archive")"
else
  die "sha256sum or shasum is required to verify the download"
fi
[[ "${actual%% *}" == "$expected" ]] || die "release archive checksum verification failed"

# Extract only the fixed executable, never archive-controlled filesystem paths.
tar -xzOf "$work_dir/$archive" "klms-${version}-${target}/klms" > "$work_dir/klms"
chmod 0755 "$work_dir/klms"
[[ "$("$work_dir/klms" --version)" == "klms ${version#v}" ]] || die "candidate version does not match release"
install_dir="${KLMS_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$install_dir"
install_dir="$(cd "$install_dir" && pwd -P)"
# The candidate preflights and installs its own embedded skill before switching
# the binary, rolling skill content back if the final atomic rename fails.
"$work_dir/klms" __install --destination "$install_dir/klms" || die "candidate installation failed (requires klms 0.2.1 or newer)"
