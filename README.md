# Forti-VPN

A lightweight Ubuntu GUI for FortiClient VPN, with simple connection control, profile management, and top-bar integration.

## Features

- Connect and disconnect with password and MFA support.
- Create, view, edit, and delete VPN profiles.
- Keep the app in the top bar when closing.
- Choose light, dark, or system appearance.
- View and copy session diagnostics.

## Install

Built for Ubuntu 26.04. FortiClient is required. If its CLI is missing, the app guides you through downloading, installing, and checking it.

```bash
sudo apt install python3 python3-gi gir1.2-gtk-4.0 gir1.2-adw-1

git clone https://github.com/othman2408/forti-vpn.git
cd forti-vpn
./scripts/install.sh
```

Open **Forti VPN** from your applications menu. Use **Set Up FortiClient** if needed; installation commands are copied for you to run in Terminal. Run the installer again after updating the source.

## Use

Select a profile and click **Connect**. Enter your password and MFA code when prompted.

Use **Manage Profile** for profile actions and **Settings** for preferences and diagnostics.

Closing the window keeps the app in the top bar by default. Use **Disconnect and Quit** to disconnect and exit, or change the close behavior in Settings. Top-bar integration requires AppIndicator support.

FortiClient manages the VPN connection and saved passwords. The app does not store passwords or MFA codes.
