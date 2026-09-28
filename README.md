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

Building by hand needs Rust 1.88 or newer and nothing else:

```sh
cargo build --release
./target/release/omarchy-sysinfo
```

## Use

| key | |
|---|---|
| `↑` `↓` or `j` `k` | previous or next section (or scroll, in the detail pane) |
| `g` / `G` | jump to the first or last section |
| `←` `→` or `h` `l` | move between the sidebar and the detail pane |
| `tab` | swap panes |
| `pgup` / `pgdn` | scroll the detail pane by a page |
| `/` | filter rows by substring |
| `r` | re-read `/proc` and `/sys` right now |
| `?` | help overlay |
| `q` or `esc` | quit |

The colours come from whichever Omarchy theme is active (including themes you
installed under `~/.config/omarchy/themes`), so it does not look out of place
next to your terminal. It never writes to `/sys` and never changes a setting.

The machine is read on a background thread, so a slow or hung source (a dead
network mount, a wedged driver) delays the next refresh instead of freezing the
screen. Every helper program it asks gets a two-second deadline. Every string
it shows is stripped of control characters first, so a device that names
itself with an escape sequence cannot reach your terminal.

### Plain output

```sh
omarchy-sysinfo --plain
```

Prints every section as plain text and exits, with no escape sequences, so it
pipes cleanly. Running it without `--plain` from a pipe does the same:

```sh
omarchy-sysinfo --plain | grep -A4 '^MEMORY'
omarchy-sysinfo --plain > sysinfo.txt
```

| flag | |
|---|---|
| *(none)* | interactive TUI |
| `--plain` | print every section and exit |
| `--plain --identifiers` | the same, including the hostname, serials, UUIDs, MAC addresses, network names and the focused window title |
| `--version` | print the version |
| `--help` | usage |

The plain report is meant for pasting into bug reports, so values that
identify your machine or what you are doing are shown as `<hidden>` unless you
pass `--identifiers`. A bad flag exits with status 2; a report that could not
be written (a full disk) exits with status 1.

## What it reports

Overview, OS & kernel, CPU, Memory, Board & firmware, Graphics, Displays,
Disks, PCI devices, USB, Wireless (when there is a radio), Sensors, Power &
battery, and Omarchy.

Almost everything comes straight from `/proc`, `/sys` and pacman's local
database. Disk usage comes from `statvfs`, btrfs details from `/sys/fs/btrfs`,
and the monitor layout from Hyprland's control socket. The only programs it
runs are `iw dev <iface> link` (wireless link details), `omarchy-channel-current`
and `omarchy menu keybindings --print` (once, at start), and `stty` for raw
mode, which it restores on the way out, on error, and on panic. It never
connects to D-Bus, never writes, and never changes settings.

Disks are followed through device-mapper, so a LUKS or LVM root (Omarchy's
default install) is credited to the partition it lives on.

## No dependencies

There are none. The cell renderer, the escape-sequence keyboard decoder, every
`/proc` and `/sys` parser, and the theme reader are written against the standard
library only. Nothing to vendor, nothing to audit but this repository, and the
release binary is a single stripped file.

The tests are the other half of that. They cover the keyboard decoder against
real byte sequences, every collector against fixture `/proc` and `/sys` trees
(a discrete GPU behind PCIe bridges, LUKS on NVMe, a wireless mouse's
battery), the renderer at every terminal size down to zero, and the binary's
command-line contract. None of them depend on the machine they run on.

```sh
cargo test
```

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` for
x86-64 and aarch64 Linux, the tests, and a build on the minimum supported Rust
version. `prek install` (or `pre-commit install`) runs the first two before
every commit. The lint set lives in `Cargo.toml`.

`tools/pty-shot.py` runs the TUI inside a pty and prints what a terminal would
show, which is how the layout gets checked:

```sh
python3 tools/pty-shot.py target/release/omarchy-sysinfo "jjj?"
```

## License

MIT. See [LICENSE](LICENSE).
