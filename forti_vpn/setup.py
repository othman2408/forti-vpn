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
        self.showing_help = False
        self.pages = Gtk.Stack(vhomogeneous=False)
        self.body.append(self.pages)

        ready = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                        margin_top=12, margin_bottom=12)
        ready.append(Gtk.Image(icon_name='emblem-ok-symbolic', pixel_size=48,
                               css_classes=['success']))
        title = label('FortiClient is ready', 'title-2')
        title.set_xalign(.5)
        ready.append(title)
        description = label('You can now manage your VPN profiles and connect.')
        description.set_xalign(.5)
        ready.append(description)
        help_button = button('Installation Help', self.show_help, 'flat')
        help_button.set_halign(Gtk.Align.CENTER)
        ready.append(help_button)
        self.pages.add_named(ready, 'ready')

        recovery = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                           margin_top=12, margin_bottom=12)
        self.recovery_title = label('FortiClient needs attention', 'title-3')
        self.recovery_details = label('')
        self.recovery_details.set_selectable(True)
        recovery.append(self.recovery_title)
        recovery.append(self.recovery_details)
        self.recovery_help = button('Installation Help', self.show_help, 'flat')
        recovery.append(self.recovery_help)
        self.pages.add_named(recovery, 'recovery')

        install = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24)
        self.install_title = label('Install FortiClient to get started.', 'title-3')
        install.append(self.install_title)
        download = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        download.append(label('Download', 'heading'))
        download.append(label(package_description(), 'dim-label'))
        download.append(self.link('Download FortiClient', DOWNLOAD_URL))
        install.append(download)
        self.download_section = download

        package = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        package.append(label('Install', 'heading'))
        self.select_button = button('Select Installer…', self.choose_package)
        package.append(self.select_button)
        self.command_panel = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                                     visible=False)
        self.command_panel.append(label('Run the copied command in Terminal.', 'dim-label'))
        self.copy_button = button('Copy Install Command', self.copy_command)
        self.command_panel.append(self.copy_button)
        self.command_label = label('')
        self.command_label.set_selectable(True)
        self.command_label.add_css_class('monospace')
        self.command_label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        command_details = Gtk.Expander(label='Show command')
        command_details.set_child(self.command_label)
        self.command_panel.append(command_details)
        package.append(self.command_panel)
        install.append(package)

        verify = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        verify.append(label('Check', 'heading'))
        verify.append(label('Finish setup in FortiClient, then check again.', 'dim-label'))
        install.append(verify)
        details = Gtk.Expander(label='Need help?')
        help_content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_top=12)
        help_content.append(label('For a work VPN, use the installer your IT team provides. '
                                  'This app supports the FortiClient 7.4.3 VPN CLI.', 'dim-label'))
        help_content.append(self.link('Installation Guide', HELP_URL))
        details.set_child(help_content)
        install.append(details)
        self.pages.add_named(install, 'install')

        self.check_status = label('')
        self.check_status.set_selectable(True)
        self.footer.insert_child_after(self.check_status, self.error)
        self.check = button('Check Again', self.check_again, 'suggested-action')
        self.close_button = button('Close', lambda *_: self.close())
        self.set_actions(self.close_button, self.check)
        self.actions.set_homogeneous(False)
        self.close_button.set_hexpand(True)
        self.check.set_hexpand(True)
        self.connect('closed', self.closed)
        self.update()
        self.set_focus(self.close_button if self.pages.get_visible_child_name() == 'ready' else self.check)

    def show_help(self, *_):
        self.showing_help = True
        self.update()

    def check_again(self, *_):
        self.showing_help = False
        self.parent.workflow.refresh()

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
        refreshing = self.parent.workflow.refreshing
        ready = not state.setup_required and state.status.state != 'unavailable'
        page = ('install' if self.showing_help or state.setup_required
                else 'ready' if ready else 'recovery')
        self.pages.set_visible_child_name(page)
        self.recovery_title.set_text('Checking FortiClient…' if refreshing else 'FortiClient needs attention')
        self.recovery_details.set_text('Checking the CLI and VPN service.' if refreshing else
            'The CLI was found, but the VPN is not ready. Open FortiClient to finish setup, '
            'or ask your IT team to check its service.')
        self.recovery_help.set_visible(not refreshing)
        ready_page = self.pages.get_visible_child_name() == 'ready'
        self.check.set_visible(not ready_page or refreshing)
        self.close_button.remove_css_class('suggested-action')
        if ready_page:
            self.close_button.add_css_class('suggested-action')
        self.check_status.set_visible(not ready_page or refreshing)
        if refreshing:
            message = 'Checking FortiClient…'
        elif state.setup_required:
            message = 'FortiClient CLI has not been found yet.'
        elif not ready:
            message = 'FortiClient is not responding. Open it to finish setup, then check again.'
        else:
            message = 'FortiClient is ready.'
        if self.check_status.get_text() != message:
            self.check_status.set_text(message)
            self.check_status.announce(message, Gtk.AccessibleAnnouncementPriority.MEDIUM)
        # Recalculate height after switching between compact and installation pages.
        self.set_content_height(-1)

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
        self.showing_help = True
        self.command_panel.set_visible(True)
        self.download_section.set_visible(False)
        self.install_title.set_text('Installer selected')
        self.select_button.set_label('Change Installer…')
        self.update()
        self.copy_button.set_sensitive(True)
        self.copy_button.set_label('Copy Install Command')
        self.show_error(self.error, '')

    def copy_command(self, *_):
        if self.command:
            self.get_clipboard().set(self.command)
            self.copy_button.set_label('Command Copied')
            self.copy_button.announce('Install command copied', Gtk.AccessibleAnnouncementPriority.MEDIUM)
