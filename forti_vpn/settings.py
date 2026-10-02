"""Native preferences and session diagnostics."""
from gi.repository import Adw, Gio, GLib, Gtk
from .support import probe_client, troubleshooting_report


class SettingsDialog(Adw.PreferencesDialog):
    def __init__(self, parent):
        super().__init__(title='Settings', content_width=540, content_height=620)
        self.parent = parent
        self.set_search_enabled(False)
        general = Adw.PreferencesPage(title='General', icon_name='preferences-system-symbolic')
        self.add(general)
        behavior = Adw.PreferencesGroup(title='Window behavior', description='Quit always disconnects the VPN before exiting.')
        self.close_behavior = Adw.ComboRow(title='On window close', model=Gtk.StringList.new([
            'Keep in top bar', 'Disconnect and quit']))
        self.close_behavior.set_selected(0 if parent.preferences.data['close_behavior'] == 'hide' else 1)
        self.close_behavior.connect('notify::selected', lambda row, *_: self.store('close_behavior', ('hide', 'quit')[row.get_selected()]))
        behavior.add(self.close_behavior)
        self.indicator_status = Adw.ActionRow(title='Top-bar integration')
        behavior.add(self.indicator_status)
        general.add(behavior)
        appearance = Adw.PreferencesGroup(title='Appearance')
        self.appearance = Adw.ComboRow(title='Color scheme', model=Gtk.StringList.new(['Follow system', 'Light', 'Dark']))
        self.appearance.set_selected(('system', 'light', 'dark').index(parent.preferences.data['appearance']))
        self.appearance.connect('notify::selected', lambda row, *_: self.store('appearance', ('system', 'light', 'dark')[row.get_selected()]))
        appearance.add(self.appearance)
        general.add(appearance)
        auth = Adw.PreferencesGroup(title='Sign in')
        self.remember = Adw.SwitchRow(title='Remember password by default', subtitle='Uses FortiClient credential storage. Existing saved passwords are not erased when this is off.')
        self.remember.set_active(parent.preferences.data['remember_password'])
        self.remember.connect('notify::active', lambda row, *_: self.store('remember_password', row.get_active()))
        auth.add(self.remember)
        general.add(auth)
        self.diagnostics_page = Adw.PreferencesPage(title='Diagnostics', icon_name='utilities-terminal-symbolic')
        self.add(self.diagnostics_page)
        system = Adw.PreferencesGroup(title='Compatibility', description='Designed for Ubuntu 26.04 and FortiClient 7.4.3.')
        self.client_info = {'version': 'Checking…', 'path': 'Checking…'}
        self.client_row = Adw.ActionRow(title='FortiClient', subtitle='Checking installed version…')
        self.cli_row = Adw.ActionRow(title='CLI executable', subtitle='Checking…')
        system.add(self.client_row)
        system.add(self.cli_row)
        self.report_button = Gtk.Button(label='Copy troubleshooting report', sensitive=False,
                                        margin_top=12, tooltip_text='Copy versions, connection state, and sanitized session events')
        self.report_button.connect('clicked', self.copy_report)
        system.add(self.report_button)
        self.diagnostics_page.add(system)
        group = Adw.PreferencesGroup(title='Session log', description='Operation events only. Passwords, codes, and raw terminal output are excluded.')
        self.text = Gtk.TextView(editable=False, cursor_visible=True, monospace=True, wrap_mode=Gtk.WrapMode.WORD_CHAR)
        self.text.update_property([Gtk.AccessibleProperty.LABEL], ['Sanitized session diagnostics'])
        self.text.set_top_margin(12)
        self.text.set_bottom_margin(12)
        self.text.set_left_margin(12)
        self.text.set_right_margin(12)
        scroll = Gtk.ScrolledWindow(min_content_height=180, hscrollbar_policy=Gtk.PolicyType.NEVER)
        scroll.add_css_class('card')
        scroll.set_child(self.text)
        group.add(scroll)
        actions = Gtk.Box(spacing=8, homogeneous=True, margin_top=12)
        for title, icon, callback in [('Copy', 'edit-copy-symbolic', self.copy), ('Save', 'document-save-symbolic', self.save), ('Clear', 'edit-clear-all-symbolic', self.clear)]:
            widget = Gtk.Button()
            content = Gtk.Box(spacing=6, halign=Gtk.Align.CENTER)
            content.append(Gtk.Image(icon_name=icon))
            content.append(Gtk.Label(label=title))
            widget.set_child(content)
            widget.set_tooltip_text(title + ' diagnostics')
            widget.update_property([Gtk.AccessibleProperty.LABEL], [title + ' diagnostics'])
            widget.connect('clicked', callback)
            actions.append(widget)
        group.add(actions)
        self.diagnostics_page.add(group)
        self.update()
        probe_client(parent, self.client_detected)

    def client_detected(self, info):
        self.client_info = info
        self.client_row.set_subtitle(info['version'])
        self.cli_row.set_subtitle(GLib.markup_escape_text(info['path']))
        self.report_button.set_sensitive(True)

    def copy_report(self, *_):
        self.get_clipboard().set(troubleshooting_report(self.parent, self.client_info))
        self.add_toast(Adw.Toast(title='Troubleshooting report copied', timeout=3))

    def store(self, key, value):
        try:
            self.parent.preferences.set(key, value)
            self.parent.apply_preferences()
        except (OSError, ValueError):
            self.add_toast(Adw.Toast(title='Could not save this setting.'))
            # Reflect the persisted choice instead of displaying an unsaved value.
            row = {'close_behavior': self.close_behavior, 'appearance': self.appearance, 'remember_password': self.remember}[key]
            if key == 'remember_password':
                row.set_active(self.parent.preferences.data[key])
            else:
                choices = ('hide', 'quit') if key == 'close_behavior' else ('system', 'light', 'dark')
                row.set_selected(choices.index(self.parent.preferences.data[key]))

    def update(self):
        buffer = self.text.get_buffer()
        contents = '\n'.join(self.parent.workflow.state.diagnostics)
        if buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False) != contents:
            buffer.set_text(contents)
        available = self.parent.indicator_available
        self.indicator_status.set_subtitle('Available in this desktop session' if available else 'Unavailable — the app will stay visible when you close it. Enable an AppIndicator extension to hide it.')

    def copy(self, *_):
        self.get_clipboard().set('\n'.join(self.parent.workflow.state.diagnostics))
        self.add_toast(Adw.Toast(title='Diagnostics copied', timeout=3))

    def clear(self, *_):
        self.parent.workflow.state.diagnostics.clear()
        self.update()
        self.add_toast(Adw.Toast(title='Session log cleared', timeout=3))

    def save(self, *_):
        dialog = Gtk.FileDialog(title='Save diagnostics', initial_name='forti-vpn-diagnostics.txt')
        contents = '\n'.join(self.parent.workflow.state.diagnostics).encode()
        def selected(source, result):
            try:
                file = source.save_finish(result)
                file.replace_contents(contents, None, False, Gio.FileCreateFlags.PRIVATE, None)
                self.add_toast(Adw.Toast(title='Diagnostics saved', timeout=3))
            except GLib.Error as exc:
                if not exc.matches(Gtk.DialogError.quark(), Gtk.DialogError.DISMISSED):
                    self.add_toast(Adw.Toast(title='Could not save diagnostics'))
        dialog.save(self.parent, None, selected)
