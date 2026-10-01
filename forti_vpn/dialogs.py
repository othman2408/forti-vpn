"""Adaptive dialogs with consistent focus, validation, and credential cleanup."""
from gi.repository import Adw, Gtk
from .backend import ProfileSettings, VPNError
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
        self.gateway = Adw.EntryRow(title='Remote gateway', text=existing.gateway if existing else '')
        self.gateway.set_tooltip_text('Server hostname or IP, optionally followed by :port')
        self.connection.add(self.name)
        self.connection.add(self.gateway)
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
        self.certificate.add(self.cert)
        self.certificate.add(self.path)
        self.body.append(self.certificate)
        self.username.set_sensitive(False)
        self.path.set_sensitive(False)
        self.auth.connect('notify::selected', lambda *_: self.username.set_sensitive(self.auth.get_selected() == 2))
        self.cert.connect('notify::selected', lambda *_: self.path.set_sensitive(self.cert.get_selected() in (1, 2)))
        self.body.append(label('Advanced settings remain managed by FortiClient. Password saving is offered when connecting.', 'dim-label'))
        self.cancel = button('Cancel', self.cancel_or_close)
        self.save_button = button('Save', self.save, 'suggested-action')
        self.set_actions(self.cancel, self.save_button)
        self.set_default_widget(self.save_button)
        self.set_focus(self.gateway if existing else self.name)
        for row in (self.name, self.gateway, self.username, self.path):
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
        self.gateway.set_text(existing.gateway if existing else '')
        self.username.set_text(existing.username if existing else '')
        self.auth.get_model().splice(0, 1, ['Keep existing setting' if existing else 'Prompt for credentials'])
        self.cert.get_model().splice(0, 1, ['Keep existing setting' if existing else 'None'])
        self.auth.set_selected(0)
        self.cert.set_selected(0)
        self.username.set_sensitive(False)
        self.path.set_text('')
        self.path.set_sensitive(False)
        self.set_focus(self.gateway if existing else self.name)

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
        for row in (self.name, self.gateway, self.username, self.path):
            row.remove_css_class('error')
        settings = ProfileSettings(self.name.get_text().strip(), self.gateway.get_text().strip(),
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
