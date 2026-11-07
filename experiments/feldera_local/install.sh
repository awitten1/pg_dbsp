#!/usr/bin/env bash
# Install a pinned Feldera release and its source dependencies under .tools/.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
state="$root/.tools/feldera"
version=v0.378.0
toolchain=1.99.0

if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
    echo 'This experiment currently supports Linux x86_64.' >&2
    exit 1
fi
for tool in curl unzip tar sha256sum python3 rustup java jq; do
    command -v "$tool" >/dev/null || { echo "Missing $tool" >&2; exit 1; }
done

mkdir -p "$state"
download() {
    local name="$1" expected="$2" url="$3"
    if [[ -f "$state/$name" ]] && printf '%s  %s\n' "$expected" "$state/$name" | sha256sum -c --status; then
        return
    fi
    curl -fL --retry 3 -o "$state/$name.tmp" "$url"
    printf '%s  %s\n' "$expected" "$state/$name.tmp" | sha256sum -c
    mv "$state/$name.tmp" "$state/$name"
}

download "pipeline-manager-$version.zip" \
    176dd8e00b82f8b74ca3eff75e68939798217a1c81b1271fe8b92557268fda1b \
    "https://github.com/feldera/feldera/releases/download/$version/pipeline-manager-x86_64-unknown-linux-gnu.zip"
download "sql2dbsp-jar-with-dependencies-$version.jar" \
    c6568cf8152a1216935bb6701e6dea9718dd014d8f867a55fd764d735ec13388 \
    "https://github.com/feldera/feldera/releases/download/$version/sql2dbsp-jar-with-dependencies-$version.jar"
download "feldera-$version.tar.gz" \
    3a4ff8d93319d5baeced5b5d553440f0480a3e4b55a3127dcf9db8171e5ca499 \
    "https://github.com/feldera/feldera/archive/refs/tags/$version.tar.gz"

if [[ ! -x "$state/pipeline-manager" ]]; then
    unzip -oq "$state/pipeline-manager-$version.zip" -d "$state"
    chmod +x "$state/pipeline-manager"
fi
if [[ ! -f "$state/source/Cargo.lock" ]]; then
    mkdir -p "$state/source"
    tar -xzf "$state/feldera-$version.tar.gz" -C "$state/source" --strip-components=1
fi

# The release's Kafka connector insists on a separately installed librdkafka.
# This experiment does not use Kafka. Build its vendored copy instead, keeping
# all dependencies private to .tools/feldera and avoiding a system install.
python3 - "$state/source/crates/connectors/Cargo.toml" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
source = path.read_text()
original = '    "dynamic-linking",\n    "tokio",\n'
replacement = '    "ssl",\n    "zstd",\n    "libz",\n    "tokio",\n'
if original in source:
    path.write_text(source.replace(original, replacement, 1))
elif replacement not in source:
    raise SystemExit(f'Unexpected rdkafka configuration in {path}')
PY

if ! rustc "+$toolchain" --version >/dev/null 2>&1; then
    rustup toolchain install "$toolchain" --profile minimal
fi
mkdir -p "$state/compiler" "$state/runner" "$state/demos"
# Generated Cargo crates live under this directory. Its own toolchain file
# overrides the parent pg_dbsp project's older Rust pin.
printf '[toolchain]\nchannel = "%s"\n' "$toolchain" > "$state/compiler/rust-toolchain.toml"

echo "Installed native Feldera $version in $state"
