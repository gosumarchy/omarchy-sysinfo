# omarchy-sysinfo

A TUI that describes this machine: hardware, OS, boot and Omarchy state,
section by section. Useful on its own, and handy as the thing you paste into a
bug report.

Written for [Omarchy](https://omarchy.org), but it reads `/proc` and `/sys`
directly, so most of it works on any Linux box. The Omarchy section is simply
empty elsewhere.

## Install

```sh
git clone https://github.com/gosumarchy/omarchy-sysinfo
cd omarchy-sysinfo
./install.sh
```

That builds the release binary, drops it in `~/.local/bin` (already on PATH),
and installs a desktop entry so it shows up in the launcher as **System Info**.
Nothing needs root.

Add `--bind` to also register a `SUPER + I` keybinding in
`~/.config/hypr/bindings.lua`:

```sh
./install.sh --bind
```

Building by hand needs a stable Rust toolchain and nothing else:

```sh
cargo build --release
./target/release/omarchy-sysinfo
```

## Use

| key | |
|---|---|
| `↑` `↓` or `j` `k` | previous or next section |
| `g` / `G` | jump to the first or last section |
| `←` `→` or `h` `l` | move between the sidebar and the detail pane |
| `tab` | swap panes |
| `pgup` / `pgdn` | scroll the detail pane by ten rows |
| `/` | filter rows by substring |
| `r` | re-read `/proc` and `/sys` right now |
| `?` | help overlay |
| `q` or `esc` | quit |

The colours come from whichever Omarchy theme is active, so it does not look
out of place next to your terminal. It never writes to `/sys` and never changes
a setting. Where `/proc` and `/sys` do not expose something conveniently it
asks a read-only helper instead — `ps`, `df`, `btrfs filesystem show`, `iw dev`,
`hyprctl`, `omarchy menu keybindings --print` — and `stty` for raw mode, which
it restores on the way out.

### Plain output

```sh
omarchy-sysinfo --plain
```

Prints every section as plain text and exits, with no escape sequences, so it
pipes cleanly:

```sh
omarchy-sysinfo --plain | grep -A4 '^MEMORY'
omarchy-sysinfo --plain > sysinfo.txt
```

| flag | |
|---|---|
| *(none)* | interactive TUI |
| `--plain` | print every section and exit |
| `--version` | print the version |
| `--help` | usage |

## What it reports

Overview, OS & kernel, CPU, Memory, Board & firmware, Graphics, Displays,
Disks, PCI devices, USB & wireless, Sensors, Wireless, Power & battery, and
Omarchy. It reads from `/proc` and `/sys` where possible; it also uses a
few read-only CLI tools (`ps`, `df`, `btrfs filesystem show`, `iw dev`,
`hyprctl`, `omarchy menu keybindings --print`) and `stty` for raw terminal mode.
It never connects to D-Bus, never writes, and never changes settings.

## No dependencies

There are none. The cell renderer, the escape-sequence keyboard decoder, every
`/proc` and `/sys` parser, and the theme reader are written against the standard
library only. Nothing to vendor, nothing to audit but this repository, and the
release binary is a single stripped file.

The tests are the other half of that: 392 of them, covering the keyboard
decoder against real byte sequences, the `/proc` parsers against recorded
samples, and the renderer at every terminal size down to zero.

```sh
cargo test
```

`tools/pty-shot.py` runs the TUI inside a pty and prints what a terminal would
show, which is how the layout gets checked:

```sh
python3 tools/pty-shot.py target/release/omarchy-sysinfo "jjj?"
```

## License

MIT. See [LICENSE](LICENSE).
