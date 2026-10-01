"""Guided FortiClient setup; administrator commands are copied, never executed."""
from gi.repository import Adw, Gio, GLib, Gtk, Pango
from .dialogs import ContentDialog
from .widgets import button, label
from .installation import DOWNLOAD_URL, HELP_URL, package_description, install_command


class SetupDialog(ContentDialog):
    def __init__(self, parent):
        super().__init__('Set up FortiClient', 440)
        self.parent = parent
        self.command = None
        self.body.append(label('Forti VPN uses FortiClient to manage your VPN connection. If FortiClient is already installed, skip to Check FortiClient below.'))
        download = Adw.PreferencesGroup(title='1. Download FortiClient')
        download.add(label('Choose the ' + package_description() + ' package. If this is a work VPN, use the package or version your IT team provides.'))
        download.add(self.link('Open Fortinet Downloads', DOWNLOAD_URL))
        self.body.append(download)
        self.body.append(label('This app targets the FortiClient 7.4.3 VPN CLI. Other versions or editions may have different commands.', 'dim-label'))
        install = Adw.PreferencesGroup(title='2. Install the downloaded package')
        install.add(label('Select the .deb file, copy its installation command, and run it in Terminal. Ubuntu will ask for your administrator password there.'))
        install.add(button('Select Installer…', self.choose_package))
        self.command_label = label('')
        self.command_label.set_selectable(True)
        self.command_label.add_css_class('monospace')
        self.command_label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.command_label.set_visible(False)
        install.add(self.command_label)
        self.copy_button = button('Copy Install Command', self.copy_command)
        self.copy_button.set_sensitive(False)
        install.add(self.copy_button)
        self.body.append(install)
        verify = Adw.PreferencesGroup(title='3. Check FortiClient')
        verify.add(label('Complete FortiClient’s first-run setup, then click Check Again. The app checks the VPN status and profiles without connecting.'))
        self.check_status = label('')
        self.check_status.set_selectable(True)
        self.footer.insert_child_after(self.check_status, self.error)
        verify.add(self.link('Fortinet Installation Guide', HELP_URL))
        self.body.append(verify)
        self.check = button('Check Again', lambda *_: parent.workflow.refresh(), 'suggested-action')
        close = button('Close', lambda *_: self.close())
        self.set_actions(close, self.check)
        self.actions.set_homogeneous(False)
        close.set_hexpand(True)
        self.check.set_hexpand(True)
        self.set_focus(self.check)
        self.connect('closed', self.closed)
        self.update()

    @staticmethod
    def link(title, uri):
        widget = Gtk.LinkButton(uri=uri, label=title)
        widget.get_child().set_wrap(True)
        widget.get_child().set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        return widget

    def closed(self, *_):
        if self.parent.setup_dialog is self:
            self.parent.setup_dialog = None

    def update(self):
        state = self.parent.workflow.state
        self.check.set_sensitive(not state.busy and not self.parent.workflow.refreshing)
        if self.parent.workflow.refreshing:
            message = 'Checking FortiClient…'
        elif state.setup_required:
            message = 'FortiClient CLI has not been found yet.'
        elif state.status.state == 'unavailable':
            message = state.error or 'FortiClient is installed but not responding. Open FortiClient to finish its setup, or ask your IT team for help.'
        else:
            message = 'FortiClient is ready.'
        if self.check_status.get_text() != message:
            self.check_status.set_text(message)
            self.check_status.announce(message, Gtk.AccessibleAnnouncementPriority.MEDIUM)

    def choose_package(self, *_):
        dialog = Gtk.FileDialog(title='Choose FortiClient installer')
        filter = Gtk.FileFilter(name='Debian packages (.deb)')
        filter.add_pattern('*.deb')
        filters = Gio.ListStore.new(Gtk.FileFilter)
        filters.append(filter)
        dialog.set_filters(filters)
        dialog.set_default_filter(filter)
        def selected(source, result):
            try:
                file = source.open_finish(result)
                self.set_package(file.get_path())
            except GLib.Error as exc:
                if not exc.matches(Gtk.DialogError.quark(), Gtk.DialogError.DISMISSED):
                    self.show_error(self.error, 'Could not select the installer. Try again.')
            except (ValueError, TypeError):
                self.show_error(self.error, 'Choose a local FortiClient .deb installer.')
        dialog.open(self.parent, None, selected)

    def set_package(self, path):
        self.command = install_command(path)
        self.command_label.set_text(self.command)
        self.command_label.set_visible(True)
        self.copy_button.set_sensitive(True)
        self.copy_button.set_label('Copy Install Command')
        self.show_error(self.error, '')

    def copy_command(self, *_):
        if self.command:
            self.get_clipboard().set(self.command)
            self.copy_button.set_label('Command Copied')
            self.copy_button.announce('Install command copied', Gtk.AccessibleAnnouncementPriority.MEDIUM)
