# Architecture

Forti VPN is a GTK 4 and libadwaita front end for the FortiClient VPN
command-line client. It never talks to a VPN server itself: every VPN
operation is a `forticlient` command, and everything the window shows is
computed from that command's parsed output.

## Crates

| Crate | Purpose | Ships |
| --- | --- | --- |
| `crates/forti-vpn-core` | FortiClient CLI driver, parsers, the operation workflow, what the window shows, platform detection, setup guidance, preferences, and the troubleshooting report. No GTK or GLib. | Compiled into the binary |
| `crates/forti-vpn` | The GTK app: window, dialogs, and tray. `main.rs` only calls `forti_vpn::run()`; the modules live in the library so integration tests can use them. | `forti-vpn` binary |
| `crates/forti-vpn-testkit` | A simulated `forticlient` (`fake-forticlient`) and integration tests that drive the real CLI code against it. | Never |

The Rust code is a port of the Python 0.1.0 app (git tag `v0.1.0`: the app in
`forti_vpn/` and its tests in `tests/`). Module docs name the Python file each
part came from; that version was the behavioural reference for the port.

## Workflow and presentation

The core holds all behaviour, so it is tested headlessly with
`cargo test -p forti-vpn-core`; the GTK code copies values into widgets and
forwards user actions.

- `workflow::Workflow` lives on the GTK main thread and owns the `State`.
  Each operation (refresh, connect, disconnect, save, delete) runs its
  `Backend` calls on a worker thread, which never touches the state and
  reports `Event`s through an `EventSink`. The app forwards them over an
  `async_channel` to a `glib::spawn_future_local` loop that calls
  `Workflow::handle` on the main thread.
- Workflow methods never call back into the UI. They queue `Effect`s
  (show a prompt, present an error, quit), which the app drains with
  `take_effects` after each call, so no re-entrant borrow can happen.
- `presentation::main_view` turns the state into a `MainView`: labels,
  which buttons are enabled, banners, and the status badge. The window
  renders that and nothing else.
- `backend::Backend` is the seam: `cli::Cli` implements it with the real
  executable, `demo` with invented data (`FORTI_VPN_DEMO=1`), and tests with
  in-memory fakes.

## The CLI driver

`cli::Cli` runs `forticlient` (or `/opt/forticlient/forticlient-cli`, or
`FORTI_VPN_CLI`) without a shell and with `LC_ALL=C`, so its output can be
parsed.

- Interactive commands such as `vpn connect` run on a pseudo-terminal
  (`rustix::pty`) with echo disabled, in a new session. `parse` recognizes
  the password, MFA, and confirmation prompts; the app asks the user and
  writes the answer to the PTY. Answers are `Zeroizing` secrets and never
  appear in arguments, logs, errors, or files.
- Output is matched against message tables and turned into the app's own
  sentences. Raw FortiClient output never reaches user-facing text;
  diagnostics keep a sanitized log.
- A cancelled or timed-out operation kills the command's process group.
- `host::HostRunner` starts commands natively, or from a Flatpak sandbox
  with `flatpak-spawn --host --watch-bus --env=LC_ALL=C`. Natively, a
  missing `DBUS_SESSION_BUS_ADDRESS` is replaced by the user's existing
  desktop bus socket when it is a socket the user owns, because FortiClient
  fails without the desktop session bus.

## Tray

`crates/forti-vpn/src/tray.rs` exports a StatusNotifierItem with a DBusMenu
on the session bus through GIO, and registers it with
`org.kde.StatusNotifierWatcher`. That works with KDE Plasma, Cinnamon,
Xfce, COSMIC, Budgie, Waybar, and GNOME with the AppIndicator extension.
The icon shows whether the VPN is connected
(`io.github.fortivpn.Utility-symbolic` and
`io.github.fortivpn.Utility-disconnected-symbolic`), and the app is told
when a tray host appears or disappears.

## Platform detection

`platform::Platform` reads `os-release` (the host's
`/run/host/os-release` inside Flatpak) and classifies the system:

- the family from `ID` and `ID_LIKE`: Debian (`apt`, `.deb`), Fedora
  (`dnf`, `.rpm`, including RHEL and its rebuilds), SUSE (`zypper`, `.rpm`),
  Arch (`pacman`, AUR), or unknown;
- Ubuntu-based systems (GNOME there ships an AppIndicator extension of its
  own: Ubuntu AppIndicators, or Zorin Appindicators on Zorin OS),
  enterprise rebuilds (extras from EPEL), and image-based systems
  (`/run/ostree-booted`, packages layered with `rpm-ostree`);
- the desktop (`XDG_CURRENT_DESKTOP`), session type, sandbox, CPU
  architecture, the administrator command (`sudo`, `doas`, `run0`, or
  `su -c`), and an AUR helper.

Detection never fails: unknown values get generic guidance. `setup` uses
the result to show FortiClient install commands, the service command
(`systemctl enable --now forticlient.service`), and tray help. Commands
are only shown and copied for the user to run; the app never runs them.

## Data and files

- Preferences: `~/.config/forti-vpn/preferences.json`, the same format as
  0.1.0, written atomically with mode `0600`. Passwords are never stored;
  FortiClient keeps saved passwords.
- The troubleshooting report (`report`) is an allowlist of facts: versions,
  distribution, desktop, and session, never profile names, gateways,
  credentials, or device identifiers.
- `crates/forti-vpn/build.rs` compiles the GResource bundle (style sheet,
  the app icons, and the Adwaita symbolic icons the UI uses) with
  `glib-compile-resources`, so icons resolve even when the desktop's theme
  lacks them.

## Packaging

The `Makefile` builds with cargo and installs the binary, desktop entry,
AppStream metainfo, icons, and license files. `packaging/` holds the RPM
spec, the Arch PKGBUILD, the experimental Flatpak manifest, and the Debian
package's `copyright` file; the Debian package itself comes from
`[package.metadata.deb]` in `crates/forti-vpn/Cargo.toml`.
`scripts/build-packages.sh` builds them in containers. See
[releases.md](releases.md).
