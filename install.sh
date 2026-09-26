#!/usr/bin/env bash
# Build omarchy-sysinfo and install it into the user's Omarchy session.
#
#   ./install.sh            build release, install binary + desktop entry
#   ./install.sh --bind     also add a SUPER+I keybinding to hyprland.lua
#
# Nothing here needs root: the binary lands in ~/.local/bin, which Omarchy
# already puts on PATH, and the desktop entry goes to the XDG user dir.

set -euo pipefail

source_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
apps_dir="$HOME/.local/share/applications"
binary="$bin_dir/omarchy-sysinfo"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust first, e.g. 'mise use -g rust@stable'." >&2
  exit 1
fi

echo "==> Building release binary"
cargo build --release --manifest-path "$source_dir/Cargo.toml"

echo "==> Installing to $binary"
mkdir -p "$bin_dir"
install -m 755 "$source_dir/target/release/omarchy-sysinfo" "$binary"

echo "==> Installing desktop entry"
mkdir -p "$apps_dir"
install -m 644 "$source_dir/packaging/omarchy-sysinfo.desktop" \
  "$apps_dir/omarchy-sysinfo.desktop"

if [[ ${1:-} == "--bind" ]]; then
  # Omarchy keeps personal overrides in bindings.lua; hyprland.lua is the main
  # config and appending to it is neither the documented place nor idempotent.
  bindings="$HOME/.config/hypr/bindings.lua"
  if [[ -f $bindings ]] && ! grep -q 'omarchy-sysinfo' "$bindings"; then
    echo "==> Adding SUPER+I binding to $bindings"
    backup="$bindings.bak.$(date +%s)"
    cp "$bindings" "$backup"
    # `tui` lets Omarchy run it in the default terminal; `focus` reuses the
    # window if it is already open.
    cat >>"$bindings" <<'LUA'

-- omarchy-sysinfo: read-only tour of this machine's hardware, OS and Omarchy
-- state. `tui` runs it in your terminal, `focus` reuses the window if it is
-- already open.
o.bind("SUPER + I", "System info", { tui = "omarchy-sysinfo", focus = true })
LUA
    hyprctl reload >/dev/null 2>&1 || true
    errors="$(hyprctl configerrors 2>/dev/null || true)"
    if [[ -n ${errors//[[:space:]]/} ]]; then
      echo "==> hyprland reported config errors; restoring $backup" >&2
      echo "$errors" >&2
      cp "$backup" "$bindings"
      hyprctl reload >/dev/null 2>&1 || true
      exit 1
    fi
  else
    echo "==> Skipping keybinding (already present or no bindings.lua)"
  fi
fi

cat <<EOF

Installed.

  omarchy-sysinfo        launch the TUI
  omarchy-sysinfo --plain  print the whole report and exit

It shows up in the app launcher as "System Info". Launch it from there or
press the SUPER key and type "system info".
EOF
