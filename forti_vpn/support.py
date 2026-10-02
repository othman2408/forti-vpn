"""Allowlisted system information, without device IDs or profile details."""
import os
import platform
import re
import threading
from gi.repository import Adw, GLib, Gtk
from . import __version__, REPOSITORY
from .backend import VPNError


def client_information(cli):
    try:
        return cli.information()
    except (VPNError, OSError, AttributeError):
        return {'version': 'Unavailable', 'path': 'Not detected'}


def probe_client(parent, callback):
    """One bounded read per dialog opening; no polling or VPN operations."""
    def run():
        info = client_information(parent.workflow.cli)
        GLib.idle_add(deliver, info)
    def deliver(info):
        if not parent.workflow.closed:
            callback(info)
        return GLib.SOURCE_REMOVE
    threading.Thread(target=run, daemon=True).start()


def troubleshooting_report(parent, info):
    try:
        system = platform.freedesktop_os_release().get('PRETTY_NAME', 'Linux')
    except OSError:
        system = 'Linux'
    gtk = '.'.join(str(value()) for value in (Gtk.get_major_version, Gtk.get_minor_version, Gtk.get_micro_version))
    adw = '.'.join(str(value()) for value in (Adw.get_major_version, Adw.get_minor_version, Adw.get_micro_version))
    desktop = os.environ.get('XDG_CURRENT_DESKTOP', 'Unknown')
    # Desktop names are a small token, not a dump of the process environment.
    if not re.fullmatch(r'[\w:;-]{1,80}', desktop):
        desktop = 'Unknown'
    state = parent.workflow.state
    status = state.status.state if state.status.state in (
        'disconnected', 'connecting', 'awaiting authentication', 'connected', 'disconnecting', 'unavailable') else 'unknown'
    return '\n'.join([
        f'Forti-VPN {__version__}', 'Unofficial FortiClient GUI',
        f'System: {system}', f'Architecture: {platform.machine()}', f'Desktop: {desktop}',
        f'GTK: {gtk}', f'libadwaita: {adw}', f'FortiClient: {info["version"]}',
        f'CLI: {info["path"]}', 'Target: Ubuntu 26.04 / FortiClient 7.4.3',
        f'VPN status: {status}', f'Top-bar integration: {"Available" if parent.indicator_available else "Unavailable"}',
        '', 'Sanitized session events (no profiles, credentials, or device identifiers):',
        *state.diagnostics, '', f'Support: {REPOSITORY}/issues',
    ])


def show_about(parent):
    dialog = Adw.AboutDialog(application_name='Forti-VPN', application_icon='io.github.fortivpn.Utility',
        developer_name='othman2408', version=__version__, website=REPOSITORY,
        issue_url=REPOSITORY + '/issues',
        comments='An unofficial native GUI for FortiClient VPN. Not affiliated with Fortinet.\n\n'
                 'Designed for Ubuntu 26.04 and FortiClient 7.4.3. FortiClient must be installed separately.')
    dialog.add_link('Fortinet downloads', 'https://www.fortinet.com/support/product-downloads')
    dialog.present(parent)
    def detected(info):
        if dialog.get_root() is not None:
            dialog.set_comments('An unofficial native GUI for FortiClient VPN. Not affiliated with Fortinet.\n\n'
                'Designed for Ubuntu 26.04 and FortiClient 7.4.3.\n'
                f'Installed FortiClient: {info["version"]}\nCLI: {info["path"]}')
    probe_client(parent, detected)
