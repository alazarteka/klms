#!/usr/bin/env bash
set -euo pipefail

# Offline regression: a corrupt download must fail before tar or tool execution.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/klms-supply-test.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
mkdir "$work_dir/bin"
export TEST_SUPPLY_DIR="$work_dir"
cat > "$work_dir/bin/uname" <<'SH'
#!/usr/bin/env bash
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac
SH
cat > "$work_dir/bin/curl" <<'SH'
#!/usr/bin/env bash
while [[ $# -gt 0 ]]; do
  if [[ "$1" == --output ]]; then
    printf 'corrupted downloaded archive\n' > "$2"
    exit 0
  fi
  shift
done
exit 2
SH
cat > "$work_dir/bin/tar" <<'SH'
#!/usr/bin/env bash
touch "$TEST_SUPPLY_DIR/extraction-attempted"
exit 99
SH
chmod +x "$work_dir/bin/"*
for tool in deny vet; do
  if PATH="$work_dir/bin:$PATH" "$script_dir/supply-chain.sh" "$tool" \
      > "$work_dir/log" 2>&1; then
    echo "Corrupt $tool archive was accepted" >&2
    exit 1
  fi
  grep -q "cargo-$tool archive checksum mismatch" "$work_dir/log"
  test ! -e "$work_dir/extraction-attempted"
done
echo 'Corrupt tool archives rejected before extraction or execution'
