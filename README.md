# Forti-VPN

A lightweight native Ubuntu GUI for FortiClient VPN, with easy connection control, profile management, and top-bar integration. Built with Python, GTK 4, and libadwaita.

## Features

- Connect and disconnect with password and MFA prompts.
- Create, view, edit, and delete VPN profiles.
- Keep the app in the top bar or disconnect and quit when closing.
- Follow the system appearance, or choose light or dark mode.
- View, copy, and save sanitized session diagnostics.

FortiClient manages the VPN tunnel and saved passwords. The app does not store passwords or MFA codes.

## Install

Requires an existing FortiClient installation. Developed for Ubuntu 26.04 and FortiClient 7.4.3.5411.

Install the GUI dependencies:

```bash
sudo apt install python3 python3-gi gir1.2-gtk-4.0 gir1.2-adw-1
```

Install the app and desktop launcher:

```bash
git clone https://github.com/othman2408/forti-vpn.git
cd forti-vpn
./scripts/install.sh
```

Open **Forti VPN** from your applications menu. Top-bar integration requires your desktop's AppIndicator support. Run the installer again after updating the source.

To run directly from the project directory:

```bash
/usr/bin/python3 -m forti_vpn
```

Run the app as your normal user, without `sudo`.

## Daily use

Select a profile and press **Connect**. Enter your password and MFA code when prompted. Use **Manage Profile** for profile actions and **Settings** for preferences and diagnostics.

Closing the window keeps the app running in the top bar by default. Use **Disconnect and Quit**, or **Ctrl+Q**, to disconnect and exit. You can change the close behavior in Settings. Force-killing the app can leave the VPN connected.

## Tests

Run the automated tests with a fake CLI:

```bash
/usr/bin/python3 -m unittest discover -s tests -v
```

Include native UI checks in a graphical session:

```bash
RUN_GUI_TESTS=1 /usr/bin/python3 -m unittest discover -s tests -v
```

Live VPN/MFA verification and a full screen-reader check are still pending. CLI changes in other FortiClient versions may require adapter updates.
