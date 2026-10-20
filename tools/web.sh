#!/usr/bin/env bash
# Builds an example for the browser and serves it on http://localhost:8000:
#
#   tools/web.sh [example] [cargo args...]        # default: viewer, the `web` profile (Cargo.toml)
#   tools/web.sh walk --profile dev               # any cargo flag passes through
#
# Needs the wasm32-unknown-unknown target and wasm-bindgen 0.2.129 (the version Cargo.lock pins);
# devenv.nix provides both. Cargo runs the result through tools/web-run.sh (.cargo/config.toml).
# Examples that read files or spawn threads themselves (fields, viability, bench) are native-only;
# the ones that take command-line arguments use their defaults in the browser.
set -euo pipefail
cd "$(dirname "$0")/.."
example=${1:-viewer}
[ $# -gt 0 ] && shift
profile=(--profile web)
for arg in "$@"; do
	case $arg in
		--release | --profile | --profile=*) profile=() ;;
	esac
done
# The devenv RUSTFLAGS ask for mold, which the wasm linker doesn't take; without the variable cargo
# falls back to .cargo/config.toml.
env -u RUSTFLAGS cargo run --target wasm32-unknown-unknown --features web "${profile[@]}" --example "$example" "$@"
