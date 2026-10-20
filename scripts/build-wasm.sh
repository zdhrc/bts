#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Install once: rustup target add wasm32-unknown-unknown
# cargo install wasm-bindgen-cli --version 0.2.126 --locked
if [[ $(wasm-bindgen --version) != 'wasm-bindgen 0.2.126' ]]; then
  echo 'requires wasm-bindgen-cli 0.2.126 (matching Cargo.lock)' >&2
  exit 1
fi
cargo build --locked --release --lib --target wasm32-unknown-unknown --no-default-features --features wasm
version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
package="bts-v${version}-wasm"
output="target/wasm/${package}"
mkdir -p "$output"
wasm-bindgen --target web --typescript --out-name bts --out-dir "$output" target/wasm32-unknown-unknown/release/bts.wasm
cp LICENSE "$output/"
cp web/example.ts "$output/"
cp web/README.md "$output/README.md"
python3 - "$output" "$version" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
(path / 'package.json').write_text(json.dumps({
    'name': 'bts-wasm', 'version': sys.argv[2], 'type': 'module',
    'private': True, 'main': './bts.js', 'types': './bts.d.ts',
    'license': 'AGPL-3.0-only', 'files': ['bts.js', 'bts.d.ts', 'bts_bg.wasm', 'bts_bg.wasm.d.ts', 'LICENSE', 'README.md', 'example.ts']
}, indent=2) + '\n')
PY
tar -czf "target/wasm/${package}.tar.gz" -C target/wasm "$package"
(cd target/wasm && shasum -a 256 "${package}.tar.gz" > "${package}.tar.gz.sha256")
echo "Built target/wasm/${package}.tar.gz"
