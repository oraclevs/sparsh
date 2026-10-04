#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <toolchain.tar.gz|https-url> [sha256]" >&2
  exit 2
}
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
source_archive=$1
expected=${2:-}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

case "$source_archive" in
  https://*)
    [ -n "$expected" ] || { echo "a SHA-256 checksum is required for downloads" >&2; exit 2; }
    command -v curl >/dev/null || { echo "curl is required to download the archive" >&2; exit 2; }
    curl --fail --location --retry 3 --output "$stage/toolchain.tar.gz" "$source_archive"
    archive="$stage/toolchain.tar.gz"
    ;;
  *)
    archive=$source_archive
    [ -f "$archive" ] || { echo "archive not found: $archive" >&2; exit 2; }
    if [ -z "$expected" ] && [ -f "$archive.sha256" ]; then
      read -r expected _ < "$archive.sha256"
    fi
    [ -n "$expected" ] || { echo "a SHA-256 checksum or .sha256 sidecar is required" >&2; exit 2; }
    ;;
esac
case "$expected" in
  *[!0-9a-fA-F]*|'') echo "invalid SHA-256 checksum" >&2; exit 2 ;;
esac
[ "${#expected}" -eq 64 ] || { echo "invalid SHA-256 checksum length" >&2; exit 2; }
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$archive" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$archive" | cut -d' ' -f1)
fi
[ "$actual" = "${expected,,}" ] || { echo "archive SHA-256 mismatch" >&2; exit 1; }

mkdir "$stage/extract"
tar -tzf "$archive" | while IFS= read -r member; do
  case "$member" in
    /*|../*|*/../*|*/..|*'//'*)
      echo "unsafe archive member: $member" >&2
      exit 1
      ;;
  esac
done
tar -xzf "$archive" -C "$stage/extract"
bundle=$(find "$stage/extract" -mindepth 1 -maxdepth 1 -type d -print -quit)
[ -n "$bundle" ] || { echo "toolchain directory missing" >&2; exit 1; }
for binary in spar spar-ls sparsh; do
  [ -f "$bundle/bin/$binary" ] || { echo "missing binary: $binary" >&2; exit 1; }
done
[ -f "$bundle/share/spar/stdlib/src/lib.spar" ] || {
  echo "matching standard library sources missing" >&2
  exit 1
}

bin_dir=${SPAR_BIN_DIR:-"$HOME/.local/bin"}
if [ -n "${SPA_HOME:-}" ]; then
  data_dir=$SPA_HOME
else
  data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}/spar
fi
mkdir -p "$bin_dir" "$data_dir"
for binary in spar spar-ls sparsh; do
  install -m 755 "$bundle/bin/$binary" "$bin_dir/$binary"
done
if [ -e "$data_dir/stdlib" ]; then
  backup="$data_dir/stdlib.backup.$(date +%Y%m%d%H%M%S)"
  mv "$data_dir/stdlib" "$backup"
  echo "previous standard library saved at $backup"
fi
cp -R "$bundle/share/spar/stdlib" "$data_dir/stdlib"
"$bin_dir/spar" --version
"$bin_dir/sparsh" --version
echo "Installed binaries in $bin_dir"
echo "Installed standard library sources in $data_dir/stdlib"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add $bin_dir to PATH to run spar, spar-ls, and sparsh." ;;
esac
