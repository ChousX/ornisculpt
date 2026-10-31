#!/usr/bin/env bash
# Runs the bench (examples/bench.rs) on a Steam Deck and brings the results back:
#
#   tools/deck-bench.sh <gen|hero|motion|crowd|all> [bench args...]   # build, install, run, fetch
#   tools/deck-bench.sh install                                       # build and install only
#
# Results land in bench/results/$RESULTS (default deck-<short sha>), as they do on the Deck.
# The bench refuses to run on battery: plug the Deck in, or pass --allow-battery.
#
# The checkout must be clean: the Deck gets this commit as its own checkout (assets, git state),
# and the binary built from it. The Deck is x86_64, so on another architecture the build happens
# on $BUILD_HOST: HEAD is pushed to its clone and built in a worktree there, inside that clone's
# devenv. The package (tools/deck/bench.sh says how it runs) is copied back here, then on.
#
#   DECK        ssh target of the Deck              (deck@steamdeck)
#   BUILD_HOST  x86_64 host to build on, if not here (toth)
#   BUILD_REPO  its clone, relative to ~            (Projects/ornisculpt)
#   BUILD_TREE  the worktree to build in            (Projects/ornisculpt-deck)
#   RESULTS     results directory name              (deck-<short sha>)
set -euo pipefail
cd "$(dirname "$0")/.."
DECK=${DECK:-deck@steamdeck}
BUILD_HOST=${BUILD_HOST:-toth}
BUILD_REPO=${BUILD_REPO:-Projects/ornisculpt}
BUILD_TREE=${BUILD_TREE:-Projects/ornisculpt-deck}
PKG=target/deck/ornisculpt-bench

# Builds the bench here and lays out $PKG: the binary, the glibc and C++ runtime it was linked
# against (SteamOS's glibc is older), and the launcher.
package() {
	[ "$(uname -m)" = x86_64 ] || { echo "package: needs an x86_64 host" >&2; exit 1; }
	cargo build --release --example bench
	local bin=target/release/examples/bench
	lib() { ldd "$bin" | awk -v l="$1" '$1 == l { print $3 }'; }
	rm -rf "$PKG"
	mkdir -p "$PKG/bin" "$PKG/lib"
	cp "$bin" "$PKG/bin/bench"
	cp -L "$(dirname "$(lib libc.so.6)")"/*.so* "$PKG/lib/"
	cp -L "$(lib libstdc++.so.6)" "$(lib libgcc_s.so.1)" "$PKG/lib/"
	cp tools/deck/bench.sh "$PKG/bench"
	chmod +x "$PKG/bench"
}

build() {
	if [ "$(uname -m)" = x86_64 ]; then
		package
		return
	fi
	local sha
	sha=$(git rev-parse HEAD)
	git push -q -f "$BUILD_HOST:$BUILD_REPO" "HEAD:refs/heads/deck-bench"
	ssh "$BUILD_HOST" "set -e
		cd ~/$BUILD_REPO
		[ -d ~/$BUILD_TREE ] || git worktree add -q --detach ~/$BUILD_TREE $sha
		cd ~/$BUILD_TREE
		git checkout -q --detach $sha
		direnv exec ~/$BUILD_REPO tools/deck-bench.sh package"
	mkdir -p "$PKG"
	rsync -a --delete "$BUILD_HOST:$BUILD_TREE/$PKG/" "$PKG/"
}

# The package to ~/ornisculpt-bench, and this commit to ~/Projects/ornisculpt (a checkout that
# takes pushes into its working tree).
install() {
	ssh "$DECK" 'test -d Projects/ornisculpt/.git || {
		git init -q -b main Projects/ornisculpt &&
		git -C Projects/ornisculpt config receive.denyCurrentBranch updateInstead; }'
	git push -q -f "$DECK:Projects/ornisculpt" HEAD:main
	rsync -a --delete "$PKG/" "$DECK:ornisculpt-bench/"
}

case ${1:-} in
	package) package ;;
	install | gen | hero | motion | crowd | all)
		[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "commit first: the Deck runs HEAD" >&2; exit 1; }
		build
		install
		[ "$1" = install ] && exit
		results=${RESULTS:-deck-$(git rev-parse --short HEAD)}
		out=Projects/ornisculpt/bench/results/$results
		ssh "$DECK" "ornisculpt-bench/bench $(printf '%q ' "$@") --out $out"
		mkdir -p "bench/results/$results"
		rsync -a "$DECK:$out/" "bench/results/$results/"
		echo "results: bench/results/$results"
		;;
	*)
		sed -n '2,8p' "$0" >&2
		exit 1
		;;
esac
