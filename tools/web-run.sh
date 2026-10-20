#!/usr/bin/env bash
# Cargo runner for wasm32-unknown-unknown (.cargo/config.toml): binds the built .wasm with
# wasm-bindgen into target/site beside web/index.html and a link to the assets, then serves that
# directory. `ORNISCULPT_WEB_PORT` picks the port (8000); `ORNISCULPT_WASM_OPT=1` also runs wasm-opt
# (smaller and faster, a minute or two longer). Program arguments are ignored: a page has none.
set -euo pipefail
cd "$(dirname "$0")/.."
wasm=$1
out=target/site
mkdir -p "$out"
rm -f "$out"/app.js "$out"/app_bg.wasm
wasm-bindgen --target web --no-typescript --out-dir "$out" --out-name app "$wasm"
if [ "${ORNISCULPT_WASM_OPT:-0}" = 1 ]; then
	wasm-opt -O3 --all-features -o "$out/app_bg.wasm" "$out/app_bg.wasm"
fi
cp web/index.html "$out/index.html"
ln -sfn "$PWD/assets" "$out/assets"
port=${ORNISCULPT_WEB_PORT:-8000}
echo "Serving http://localhost:$port/ (needs WebGPU: current Chrome or Edge, or Firefox 141+)"
exec python3 -m http.server "$port" --bind 127.0.0.1 --directory "$out"
