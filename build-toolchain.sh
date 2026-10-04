#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/spar/Cargo.toml" | head -1)
shell_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/sparsh/Cargo.toml" | head -1)
host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
  *-unknown-linux-*|*-apple-darwin) ;;
  *) echo "sparsh toolchain archive is not supported on $host" >&2; exit 2 ;;
esac
for crate in spar spar-ls sparsh; do
  cargo build --release --manifest-path "$root/$crate/Cargo.toml"
done
name="spar-toolchain-${version}-sparsh-${shell_version}-${host}"
archive="$root/sparsh/dist/$name.tar.gz"
mkdir -p "$root/sparsh/dist"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name/bin" "$stage/$name/licenses" "$stage/$name/share/spar"
cp "$root/spar/target/release/spar" "$stage/$name/bin/"
cp "$root/spar-ls/target/release/spar-ls" "$stage/$name/bin/"
cp "$root/sparsh/target/release/sparsh" "$stage/$name/bin/"
cp -R "$root/spar/stdlib" "$stage/$name/share/spar/stdlib"
for crate in spar spar-ls sparsh; do
  cp "$root/$crate/LICENSE" "$stage/$name/licenses/$crate-LICENSE"
done
"$stage/$name/bin/spar" --version
"$stage/$name/bin/sparsh" --version
cat > "$stage/$name/README.txt" <<README
Spar toolchain ${version}; Sparsh ${shell_version}; target ${host}

Add bin/ to PATH, or copy its three executables to an existing PATH directory.
This archive contains spar, spar-ls, sparsh, and the matching standard library sources. Native packages are distributed separately.
README
tar -C "$stage" -czf "$archive" "$name"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$root/sparsh/dist" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
  (cd "$root/sparsh/dist" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi
echo "$archive"
