#!/usr/bin/env bash
set -euo pipefail

for tool in git cargo rustc cc; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Install $tool before running the Spar beta source installer." >&2
    exit 2
  fi
done
cargo --version
rustc --version

source_root=$(mktemp -d)
trap 'rm -rf "$source_root"' EXIT
clone() {
  local name=$1 url=$2 ref=$3 attempt
  for attempt in 1 2 3; do
    echo "Cloning $name ($ref), attempt $attempt"
    if GIT_TERMINAL_PROMPT=0 git -c http.lowSpeedLimit=1024 -c http.lowSpeedTime=45 \
      clone --quiet --depth 1 --branch "$ref" "$url" "$source_root/$name.attempt.$attempt"; then
      mv "$source_root/$name.attempt.$attempt" "$source_root/$name"
      return 0
    fi
    echo "Clone of $name failed on attempt $attempt" >&2
  done
  echo "Unable to clone $name from $url at $ref" >&2
  return 1
}
clone spar "${SPAR_SOURCE_URL:-https://github.com/oraclevs/spar.git}" "${SPAR_SOURCE_REF:-beta}"
clone spar-command "${SPAR_COMMAND_SOURCE_URL:-https://github.com/oraclevs/spar-command.git}" "${SPAR_COMMAND_SOURCE_REF:-main}"
clone spar-process "${SPAR_PROCESS_SOURCE_URL:-https://github.com/oraclevs/spar-process.git}" "${SPAR_PROCESS_SOURCE_REF:-beta}"
clone scoc "${SCOC_SOURCE_URL:-https://github.com/oraclevs/scoc.git}" "${SCOC_SOURCE_REF:-main}"
clone spar-ls "${SPAR_LS_SOURCE_URL:-https://github.com/oraclevs/spar-ls.git}" "${SPAR_LS_SOURCE_REF:-beta}"
clone sparsh "${SPARSH_SOURCE_URL:-https://github.com/oraclevs/sparsh.git}" "${SPARSH_SOURCE_REF:-beta}"
clone spar-libraries "${SPAR_LIBRARIES_SOURCE_URL:-https://github.com/oraclevs/spar-libraries.git}" "${SPAR_LIBRARIES_SOURCE_REF:-main}"

if ! diff -qr "$source_root/spar/stdlib" "$source_root/spar-libraries/stdlib" >/dev/null; then
  echo "Spar and spar-libraries contain different standard library sources; install matching beta revisions." >&2
  exit 1
fi
cp -R "$source_root/spar-libraries/sdk/spar-native-sys" "$source_root/spar-native-sys"

export CARGO_TARGET_DIR=${SPAR_BUILD_TARGET_DIR:-"$source_root/target"}
for crate in spar spar-ls sparsh; do
  echo "Building $crate"
  cargo build --release --locked --manifest-path "$source_root/$crate/Cargo.toml"
done

if [ "${SPAR_SKIP_NATIVE_BUILD:-0}" != 1 ]; then
  echo "Building the TCP native package for this host"
  env -u CARGO_TARGET_DIR bash "$source_root/spar-libraries/packages/spar-tcp/build-package.sh"
fi

bin_dir=${SPAR_BIN_DIR:-"${HOME:?}/.local/bin"}
if [ -n "${SPA_HOME:-}" ]; then
  data_dir=$SPA_HOME
else
  data_dir=${XDG_DATA_HOME:-"${HOME:?}/.local/share"}/spar
fi
mkdir -p "$bin_dir" "$data_dir"
for binary in spar spar-ls sparsh; do
  install -m 755 "$CARGO_TARGET_DIR/release/$binary" "$bin_dir/$binary"
done
for item in stdlib libraries; do
  if [ -e "$data_dir/$item" ]; then
    backup="$data_dir/$item.backup.$(date +%Y%m%d%H%M%S).$$"
    mv "$data_dir/$item" "$backup"
    echo "Saved previous $item at $backup"
  fi
done
cp -R "$source_root/spar-libraries/stdlib" "$data_dir/stdlib"
cp -R "$source_root/spar-libraries/packages" "$data_dir/libraries"
{
  for repo in spar spar-command spar-process scoc spar-ls sparsh spar-libraries; do
    printf "%s %s\n" "$repo" "$(git -C "$source_root/$repo" rev-parse HEAD)"
  done
} > "$data_dir/source-revisions.txt"
"$bin_dir/spar" --version
"$bin_dir/sparsh" --version
echo "Installed binaries in $bin_dir"
echo "Installed standard library in $data_dir/stdlib"
echo "Installed package sources in $data_dir/libraries"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add $bin_dir to PATH to run spar, spar-ls, and sparsh." ;;
esac
