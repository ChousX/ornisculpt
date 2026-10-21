# Toolchain, Vulkan and system libraries come from the shared Bevy module
# (chousx/devenv/bevy, see devenv.yaml).
{ pkgs, ... }:
{
	bevy.tracy.enable = true;

	# Browser builds (see `tools/web-run.sh`): the wasm target's std, wasm-bindgen matching the
	# version Cargo.lock pins, and wasm-opt.
	languages.rust.targets = [ "wasm32-unknown-unknown" ];
	packages = [
		pkgs.wasm-bindgen-cli
		pkgs.binaryen
	];
	# meshopt compiles C++ for the wasm target through cc-rs; the nix-wrapped clang adds hardening
	# flags wasm rejects, so point it at the plain compiler and archiver.
	env.CC_wasm32_unknown_unknown = "${pkgs.llvmPackages.clang-unwrapped}/bin/clang";
	env.AR_wasm32_unknown_unknown = "${pkgs.llvmPackages.llvm}/bin/llvm-ar";

	# Shells started outside the graphical session (herdr panes, ssh) have no WAYLAND_DISPLAY, and
	# bevy_winit panics without one. Pick up the compositor's socket if it is running.
	enterShell = ''
		if [ -z "''${WAYLAND_DISPLAY:-}" ] && [ -n "''${XDG_RUNTIME_DIR:-}" ]; then
			for sock in "$XDG_RUNTIME_DIR"/wayland-[0-9]*; do
				[ -S "$sock" ] || continue
				export WAYLAND_DISPLAY="''${sock##*/}"
				break
			done
		fi
	'';
}
