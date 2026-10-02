"""Adaptive dialogs with consistent focus, validation, and credential cleanup."""
from gi.repository import Adw, Gio, GLib, Gtk
from .backend import ProfileSettings, VPNError
from .gateway import build_gateway, split_gateway
from .widgets import label, button


class ContentDialog(Adw.Dialog):
    def __init__(self, title, width=420):
        super().__init__(title=title, content_width=width)
        layout = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        layout.append(Adw.HeaderBar())
        scroll = Gtk.ScrolledWindow(hscrollbar_policy=Gtk.PolicyType.NEVER,
                                    propagate_natural_height=True, max_content_height=580, vexpand=True)
        self.body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16,
                            margin_top=18, margin_bottom=18, margin_start=18, margin_end=18)
        scroll.set_child(self.body)
        layout.append(scroll)
        self.footer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                              margin_start=18, margin_end=18, margin_top=12, margin_bottom=18, visible=False)
        self.error = label('', 'error')
        self.error.set_selectable(True)
        self.error.set_visible(False)
        self.footer.append(self.error)
        self.actions = Gtk.Box(spacing=8, homogeneous=True)
        self.footer.append(self.actions)
        layout.append(self.footer)
        self.set_child(layout)

    def set_actions(self, *buttons):
        for widget in buttons:
            self.actions.append(widget)
        self.footer.set_visible(True)

    def show_error(self, label, message):
        label.set_text(message)
        label.set_visible(bool(message))
        if message:
            label.announce(message, Gtk.AccessibleAnnouncementPriority.HIGH)


class AuthenticationDialog(ContentDialog):
    def __init__(self, parent, challenge):
        titles = {'password': 'VPN password', 'mfa': 'Verification code', 'username': 'VPN username',
                  'certificate_password': 'Client certificate password', 'verify_password': 'Confirm password'}
        super().__init__(titles[challenge.kind], 380)
        self.challenge = challenge
        self.submitted = False
        self.body.append(label('Enter the code from your authenticator.' if challenge.kind == 'mfa'
                                    else 'Enter the client certificate password.' if challenge.kind == 'certificate_password'
                                    else 'Sign in to your VPN profile.'))
        row_type = Adw.EntryRow if challenge.kind == 'username' else Adw.PasswordEntryRow
        self.entry = row_type(title=titles[challenge.kind])
        group = Adw.PreferencesGroup()
        group.add(self.entry)
        self.body.append(group)
        self.proceed = button('Continue', self.submit, 'suggested-action')
        self.proceed.set_sensitive(False)
        self.set_actions(button('Cancel', lambda *_: self.close()), self.proceed)
        self.entry.connect('entry-activated', self.submit)
        self.entry.connect('changed', self.changed)
        self.set_default_widget(self.proceed)
        self.set_focus(self.entry)
        def closed(*_):
            self.entry.set_text('')
            if not self.submitted:
                challenge.respond(None)
            if parent.challenge_window is self:
                parent.challenge_window = None
        self.connect('closed', closed)

    def changed(self, *_):
        self.proceed.set_sensitive(bool(self.entry.get_text()) and not self.submitted)
        self.entry.remove_css_class('error')
        self.error.set_visible(False)

    def submit(self, *_):
        if self.submitted:
            return
        if not self.entry.get_text():
            self.entry.add_css_class('error')
            self.show_error(self.error, 'Enter a value to continue.')
            self.entry.grab_focus()
            return
        self.submitted = True
        value = self.entry.get_text()
        self.entry.set_text('')
        self.challenge.respond(value)
        value = None
        self.close()


class ProfileEditor(ContentDialog):
    def __init__(self, parent, existing):
        super().__init__('Edit profile' if existing else 'New profile')
        self.parent = parent
        self.existing = existing
        self.saving = False
        self.connection = Adw.PreferencesGroup(title='Connection')
        self.name = Adw.EntryRow(title='Profile name', text=existing.name if existing else '')
        self.name.set_sensitive(existing is None)
        server, port = split_gateway(existing.gateway if existing else '')
        self.gateway = Adw.EntryRow(title='Remote gateway', text=server)
        self.gateway.set_tooltip_text('Server hostname or IP address')
        self.port = Adw.EntryRow(title='Port', text=port)
        self.port.set_input_purpose(Gtk.InputPurpose.DIGITS)
        self.connection.add(self.name)
        self.connection.add(self.gateway)
        self.connection.add(self.port)
        self.body.append(self.connection)
        self.authentication = Adw.PreferencesGroup(title='Authentication')
        self.auth = Adw.ComboRow(title='Mode', model=Gtk.StringList.new([
            'Keep existing setting' if existing else 'Prompt for credentials',
            'Prompt for credentials', 'Save username', 'Disabled']))
        self.username = Adw.EntryRow(title='Username', text=existing.username if existing else '')
        self.authentication.add(self.auth)
        self.authentication.add(self.username)
        self.body.append(self.authentication)
        self.certificate = Adw.PreferencesGroup(title='Client certificate')
        self.cert = Adw.ComboRow(title='Type', model=Gtk.StringList.new([
            'Keep existing setting' if existing else 'None', 'Local certificate (.p12)', 'Smartcard (PKCS#11)', 'None']))
        self.path = Adw.EntryRow(title='Certificate path or library')
        self.browse = Gtk.Button(icon_name='folder-open-symbolic', valign=Gtk.Align.CENTER,
                                 tooltip_text='Choose certificate or library', sensitive=False)
        self.browse.add_css_class('flat')
        self.browse.update_property([Gtk.AccessibleProperty.LABEL], ['Choose certificate or library'])
        self.browse.connect('clicked', self.choose_certificate)
        self.path.add_suffix(self.browse)
        self.certificate.add(self.cert)
        self.certificate.add(self.path)
        self.body.append(self.certificate)
        self.username.set_sensitive(False)
        self.path.set_sensitive(False)
        self.auth.connect('notify::selected', lambda *_: self.username.set_sensitive(self.auth.get_selected() == 2))
        self.cert.connect('notify::selected', self.certificate_changed)
        self.body.append(label('Other settings stay managed by FortiClient.', 'dim-label'))
        self.cancel = button('Cancel', self.cancel_or_close)
        self.save_button = button('Save', self.save, 'suggested-action')
        self.set_actions(self.cancel, self.save_button)
        self.set_default_widget(self.save_button)
        self.set_focus(self.gateway if existing else self.name)
        for row in (self.name, self.gateway, self.port, self.username, self.path):
            row.connect('entry-activated', self.save)
            row.connect('changed', lambda row: row.remove_css_class('error'))
        self.connect('close-attempt', self.cancel_or_close)
        self.connect('closed', self.closed)

    def configure(self, existing):
        """Populate the prepared form from the current FortiClient profile snapshot."""
        self.existing = existing
        self.set_title('Edit profile' if existing else 'New profile')
        self.name.set_sensitive(existing is None)
        self.name.set_text(existing.name if existing else '')
        server, port = split_gateway(existing.gateway if existing else '')
        self.gateway.set_text(server)
        self.port.set_text(port)
        self.username.set_text(existing.username if existing else '')
        self.auth.get_model().splice(0, 1, ['Keep existing setting' if existing else 'Prompt for credentials'])
        self.cert.get_model().splice(0, 1, ['Keep existing setting' if existing else 'None'])
        self.auth.set_selected(0)
        self.cert.set_selected(0)
        self.username.set_sensitive(False)
        self.path.set_text('')
        self.path.set_sensitive(False)
        self.set_focus(self.gateway if existing else self.name)

    def certificate_changed(self, *_):
        selected = self.cert.get_selected()
        self.path.set_sensitive(selected in (1, 2))
        self.browse.set_sensitive(selected in (1, 2))
        self.path.set_title('Certificate file' if selected == 1 else 'PKCS#11 library' if selected == 2
                            else 'Certificate path or library')

    def choose_certificate(self, *_):
        kind = self.cert.get_selected()
        chooser = Gtk.FileDialog(title='Choose client certificate' if kind == 1 else 'Choose PKCS#11 library')
        filters = Gio.ListStore.new(Gtk.FileFilter)
        preferred = Gtk.FileFilter()
        preferred.set_name('PKCS#12 certificates' if kind == 1 else 'Shared libraries')
        for pattern in (('*.p12', '*.pfx', '*.P12', '*.PFX') if kind == 1 else ('*.so', '*.so.*')):
            preferred.add_pattern(pattern)
        filters.append(preferred)
        all_files = Gtk.FileFilter()
        all_files.set_name('All files')
        all_files.add_pattern('*')
        filters.append(all_files)
        chooser.set_filters(filters)
        chooser.set_default_filter(preferred)
        def selected(source, result):
            try:
                file = source.open_finish(result)
                if self.get_root() is None or self.saving or self.cert.get_selected() != kind:
                    return
                path = file.get_path()
                if path:
                    self.path.set_text(path)
                else:
                    self.show_error(self.error, 'Choose a file stored on this computer.')
            except GLib.Error as exc:
                if not exc.matches(Gtk.DialogError.quark(), Gtk.DialogError.DISMISSED):
                    self.show_error(self.error, 'Could not open the file picker. Enter the path instead.')
        chooser.open(self.parent, None, selected)

    def closed(self, *_):
        if self.parent.profile_dialog is self:
            self.parent.profile_dialog = None
            self.parent.schedule_editor_preparation()
            if not self.parent.workflow.closed:
                self.parent.render(self.parent.workflow.state)

    def cancel_or_close(self, *_):
        if self.saving:
            self.parent.cancel_connection()
            self.cancel.set_label('Cancelling…')
            self.cancel.set_sensitive(False)
        else:
            self.close()

    def save(self, *_):
        if self.saving:
            return
        for row in (self.name, self.gateway, self.port, self.username, self.path):
            row.remove_css_class('error')
        try:
            gateway = build_gateway(self.gateway.get_text(), self.port.get_text(),
                                    self.existing.gateway if self.existing else '')
        except VPNError as exc:
            row = self.port if 'port' in str(exc).lower() else self.gateway
            row.add_css_class('error')
            self.show_error(self.error, str(exc))
            row.grab_focus()
            return
        server, port = split_gateway(gateway)
        self.gateway.set_text(server)
        self.port.set_text(port)
        settings = ProfileSettings(self.name.get_text().strip(), gateway,
            str(self.auth.get_selected()) if self.auth.get_selected() else ('' if self.existing else '1'),
            self.username.get_text(),
            str(self.cert.get_selected()) if self.cert.get_selected() else ('' if self.existing else '3'),
            self.path.get_text())
        try:
            settings.validate()
        except VPNError as exc:
            invalid = next((row for row in (self.name, self.gateway, self.username, self.path)
                            if any(c in row.get_text() for c in ('\n', '\r', '\x00'))), None)
            if invalid is None:
                invalid = (self.name if not settings.name or settings.name.startswith('-')
                           else self.gateway if not settings.gateway
                           else self.username if settings.authentication == '2' and not settings.username.strip()
                           else self.path)
            invalid.add_css_class('error')
            self.show_error(self.error, str(exc))
            invalid.grab_focus()
            return
        state = self.parent.workflow.state
        if state.busy:
            self.show_error(self.error, 'Wait for the current operation to finish, then try again.')
            return
        # The connection may have changed externally while the editor was open.
        current = next((p for p in state.profiles if self.existing and p.name == self.existing.name), None)
        if self.existing and current is None:
            self.show_error(self.error, 'This profile no longer exists. Close this dialog and create a new profile.')
            return
        locked = (current and (current.managed or
                  (state.status.state != 'disconnected' and state.status.profile in (None, current.name))))
        if locked or (self.existing is None and state.status.state != 'disconnected'):
            self.show_error(self.error, 'Disconnect the VPN before saving this profile.')
            return
        self.show_error(self.error, '')
        self.saving = True
        self.set_can_close(False)
        self.cancel.grab_focus()
        for group in (self.connection, self.authentication, self.certificate):
            group.set_sensitive(False)
        self.save_button.set_sensitive(False)
        self.save_button.set_label('Saving…')
        self.cancel.set_label('Cancel Save')
        self.parent.workflow.save(settings, self.existing)

    def finish_save(self, success):
        if not self.saving:
            return
        self.saving = False
        self.set_can_close(True)
        if success:
            self.close()
            return
        for group in (self.connection, self.authentication, self.certificate):
            group.set_sensitive(True)
        self.save_button.set_label('Retry Save')
        self.save_button.set_sensitive(True)
        self.cancel.set_label('Cancel')
        self.cancel.set_sensitive(True)
        self.show_error(self.error, self.parent.workflow.state.error)
        self.save_button.grab_focus()
