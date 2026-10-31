#!/bin/sh
# The bench on SteamOS, installed as ~/ornisculpt-bench/bench by tools/deck-bench.sh.
#
# The binary is built with nix, against a newer glibc than SteamOS's, so it runs under the loader
# and C/C++ runtime bundled in lib/ and takes everything else from SteamOS (/usr/lib): Vulkan, the
# GPU driver, Wayland and udev. --library-path covers only the bench itself, so the git it runs
# still gets the system libraries.
#
# `bench all` would re-run its own executable, which here is the loader, so this runs each
# scenario itself, each in its own process as `all` does.
#
# Reads the checkout at ORNISCULPT_BENCH_ROOT (default ~/Projects/ornisculpt), which must be at
# the commit the binary was built from.
d=$(dirname "$(readlink -f "$0")")
export ORNISCULPT_BENCH_ROOT="${ORNISCULPT_BENCH_ROOT:-$HOME/Projects/ornisculpt}"
run() { "$d/lib/ld-linux-x86-64.so.2" --library-path "$d/lib:/usr/lib" "$d/bin/bench" "$@"; }
if [ "$1" = all ]; then
	shift
	for s in gen hero motion crowd; do run "$s" "$@" || exit; done
else
	run "$@"
fi
