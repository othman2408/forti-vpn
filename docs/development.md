# Development

## Requirements

Rust 1.92 or newer, a C compiler, pkg-config, and the development files of
GTK 4.14 and libadwaita 1.5 or newer, which bring `glib-compile-resources`:

| Distribution | Build dependencies |
| --- | --- |
| Ubuntu 24.04 and newer, Debian 13 | `sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev` |
| Fedora | `sudo dnf install gcc make pkgconf-pkg-config gtk4-devel libadwaita-devel cargo rust` |
| RHEL 10, AlmaLinux 10, Rocky Linux 10 | Enable CRB (`sudo dnf config-manager --set-enabled crb`; on RHEL, the CodeReady Builder repository), then as on Fedora |
| openSUSE Tumbleweed, Leap 16 | `sudo zypper install gcc make pkg-config gtk4-devel libadwaita-devel cargo rust` |
| Arch Linux | `sudo pacman -S --needed base-devel gtk4 libadwaita rust` |

Where the distribution's Rust is older than 1.92 (Ubuntu 24.04, Debian 13),
install Rust with [rustup](https://rustup.rs) instead of the `cargo` and
`rustc` packages.

The UI tests also need Xvfb and D-Bus (`xvfb-run`, `dbus-run-session`), and
`make validate` needs `desktop-file-validate` (desktop-file-utils) and
`appstreamcli` (appstream).

## Run

```bash
FORTI_VPN_DEMO=1 cargo run -p forti-vpn   # invented profiles and a simulated connection
cargo run -p forti-vpn                    # the installed FortiClient CLI
FORTI_VPN_CLI=/path/to/forticlient cargo run -p forti-vpn
```

Demo mode never touches the system; the screenshots in `docs/screenshots`
are taken with it. The app refuses to run as root.

## Test

```bash
cargo test -p forti-vpn-core -p forti-vpn-testkit   # headless
FORTI_VPN_UI_TESTS=1 FORTI_VPN_DBUS_TESTS=1 GSK_RENDERER=cairo \
  xvfb-run -a dbus-run-session -- cargo test -p forti-vpn
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
make validate
```

`forti-vpn-core` holds the CLI driver and all window logic, so most
behaviour is covered headlessly. The `forti-vpn-testkit` tests drive the
real CLI code against `fake-forticlient`, a simulated FortiClient. The app's
UI tests need a display (Xvfb): with `FORTI_VPN_UI_TESTS=1` they fail when
GTK cannot open one, otherwise they skip. Its tray tests need a private
session bus and run only with `FORTI_VPN_DBUS_TESTS=1` (they refuse the
desktop's own bus). `make check` runs `cargo test --workspace`; in a desktop
session that runs the UI cases on your display, so windows open and the copy
tests replace the clipboard.

`cargo run -p forti-vpn` hands off to an already running Forti VPN with the
same application ID (for example an installed one kept in the tray): quit it
first with Disconnect and Quit. `FORTI_VPN_DISABLE_INDICATOR=1` runs the app
without a tray icon.

## Other distributions in containers

Run builds as your own user with the host's rustup toolchain mounted, and a
target directory per container, since build scripts link against each
distribution's libraries:

```bash
docker run -d --name fvpn-fedora \
  -v "$PWD:$PWD" -w "$PWD" \
  -v "$HOME/.rustup:$HOME/.rustup:ro" -v "$HOME/.cargo:$HOME/.cargo" \
  -e RUSTUP_HOME="$HOME/.rustup" -e CARGO_HOME="$HOME/.cargo" \
  -e PATH="$HOME/.cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  -e CARGO_TARGET_DIR="$PWD/target/fedora" \
  fedora:latest sleep infinity
docker exec fvpn-fedora dnf install -y gcc pkgconf-pkg-config gtk4-devel libadwaita-devel \
  xorg-x11-server-Xvfb xorg-x11-xauth dbus-daemon mesa-dri-drivers
docker exec --user "$(id -u):$(id -g)" -e HOME=/tmp -e FORTI_VPN_UI_TESTS=1 \
  -e FORTI_VPN_DBUS_TESTS=1 -e GSK_RENDERER=cairo fvpn-fedora \
  xvfb-run -a dbus-run-session -- cargo test -p forti-vpn
docker rm -f fvpn-fedora
```

Podman works the same with `--userns=keep-id`. The CI workflow
(`.github/workflows/ci.yml`) runs this on Ubuntu 24.04 and 26.04, Debian 13,
Fedora, Arch Linux, and openSUSE Tumbleweed, and builds on AlmaLinux 10 with
its Rust 1.92, the minimum supported version. That job fails when
AlmaLinux's Rust stops matching `rust-version` in `Cargo.toml`, rather than
quietly testing a newer one: then install that Rust with rustup in the job
(AlmaLinux images follow the newest 10.x repositories, so pinning the image
does not pin Rust), or raise the minimum.

Packages are built in containers too:

```bash
scripts/build-packages.sh            # deb, rpm, and arch into dist/
scripts/build-packages.sh deb        # one format
```

CI builds all three when a change touches the packaging
(`.github/workflows/packages.yml`). See [releases.md](releases.md).

## Flatpak (experimental)

`packaging/flatpak/io.github.fortivpn.Utility.yml` builds against the GNOME
51 runtime. It is self-hosted and not meant for Flathub: the app runs the
host's FortiClient CLI with `flatpak-spawn --host`, which needs
`--talk-name=org.freedesktop.Flatpak`. That permission lets the app run any
command on the host, so the sandbox does not contain it. The other
permissions are the display (`wayland`, `fallback-x11`, `ipc`, `dri`), the
tray (`org.kde.StatusNotifierWatcher`), read-only Downloads for choosing a
FortiClient package, and read-only `/var/log/forticlient` for diagnostics.

To build it locally, generate the crate sources from `Cargo.lock` with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
(it needs Python with `aiohttp` and `tomlkit`), then run flatpak-builder:

```bash
python3 flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json
flatpak-builder --user --install --force-clean --install-deps-from=flathub \
  build-dir packaging/flatpak/io.github.fortivpn.Utility.yml
flatpak run io.github.fortivpn.Utility
```

`cargo-sources.json` is generated, not committed; CI generates it the same
way before building the bundle.

## Layout

See [architecture.md](architecture.md) for the crates and how they fit
together.
