"""Native GTK/libadwaita presentation."""
import os
from pathlib import Path
import gi

gi.require_version('Gtk', '4.0')
gi.require_version('Adw', '1')
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

from .preferences import Preferences
from .workflow import Workflow
from .indicator import Indicator
from .settings import SettingsDialog
from .dialogs import ContentDialog, AuthenticationDialog, ProfileEditor
from .widgets import label, button, icon_button, margins
from .setup import SetupDialog

APP_ID = 'io.github.fortivpn.Utility'


def dispatch(callback):
    def invoke():
        callback()
        return GLib.SOURCE_REMOVE
    GLib.idle_add(invoke)


class UtilityWindow(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title='Forti VPN', default_width=480, default_height=740)
        self.preferences = Preferences()
        self.settings_window = None
        self.setup_dialog = None
        self.indicator_available = False
        self.exit_requested = False
        self.cli_seen = False
        self.exit_checked = False
        self.exiting = False
        self.apply_preferences()
        self.rendering = False
        self.names = []
        self.challenge_window = None
        self.profile_dialog = None
        self.last_toast = None
        self.badge_animation = None
        self.glow_animation = None
        self.glow_visible = False
        self.workflow = Workflow(dispatch, self.render, self.show_challenge, on_complete=self.operation_finished)
        root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        toolbar = Adw.HeaderBar()
        root.append(toolbar)
        menu = Gio.Menu()
        menu.append('Settings', 'app.settings')
        menu.append('Set Up FortiClient', 'app.setup')
        menu.append('Disconnect and Quit', 'app.quit')
        menu_button = Gtk.MenuButton(icon_name='open-menu-symbolic', menu_model=menu, tooltip_text='Application menu')
        menu_button.update_property([Gtk.AccessibleProperty.LABEL], ['Application menu'])
        toolbar.pack_end(menu_button)
        refresh = Gtk.Button(icon_name='view-refresh-symbolic', tooltip_text='Refresh profiles and status')
        refresh.update_property([Gtk.AccessibleProperty.LABEL], ['Refresh profiles and status'])
        refresh.connect('clicked', lambda *_: self.workflow.refresh())
        toolbar.pack_start(refresh)
        self.refresh_button = refresh
        scroll = Gtk.ScrolledWindow(hscrollbar_policy=Gtk.PolicyType.NEVER, vexpand=True)
        backdrop = Gtk.Overlay(vexpand=True)
        self.connection_glow = Gtk.Box(valign=Gtk.Align.START, height_request=340, can_target=False)
        self.connection_glow.add_css_class('connection-glow')
        self.connection_glow.set_opacity(0)
        backdrop.set_child(self.connection_glow)
        backdrop.add_overlay(scroll)
        backdrop.set_measure_overlay(scroll, True)
        root.append(backdrop)
        clamp = Adw.Clamp(maximum_size=560)
        scroll.set_child(clamp)
        content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16)
        margins(content)
        clamp.set_child(content)
        heading = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        self.connection_header = heading
        heading.add_css_class('connection-header')
        heading.set_margin_top(8)
        heading.set_margin_bottom(8)
        self.hero_icon = Gtk.Image.new_from_file(str(Path(__file__).parent / 'assets' / (APP_ID + '.svg')))
        self.hero_connected = False
        self.hero_icon.update_property([Gtk.AccessibleProperty.LABEL], ['Forti VPN'])
        self.hero_icon.set_pixel_size(80)
        heading.append(self.hero_icon)
        title = label('Forti VPN', 'title-2')
        title.set_xalign(.5)
        heading.append(title)
        self.status_badge = Gtk.Box(spacing=6, halign=Gtk.Align.CENTER)
        self.status_badge.add_css_class('status-badge')
        self.status_icon = Gtk.Image(icon_name='network-offline-symbolic', pixel_size=16)
        self.status_badge.append(self.status_icon)
        self.status_label = label('Checking FortiClient…')
        self.status_label.set_xalign(.5)
        self.status_label.set_max_width_chars(28)
        self.status_badge.append(self.status_label)
        self.spinner = Gtk.Spinner()
        self.status_badge.append(self.spinner)
        heading.append(self.status_badge)
        content.append(heading)

        self.setup_group = Adw.PreferencesGroup(title='Get started',
            description='Install FortiClient to use VPN connections and profiles.')
        self.setup_group.add(button('Set Up FortiClient', lambda *_: self.show_setup(), 'suggested-action'))
        self.setup_group.set_visible(False)
        content.append(self.setup_group)
        profiles = Adw.PreferencesGroup(title='VPN profile')
        self.profiles_group = profiles
        self.profile_row = Adw.ComboRow(title='Profile', model=Gtk.StringList.new([]))
        self.profile_row.connect('notify::selected', self.select_profile)
        profiles.add(self.profile_row)
        content.append(profiles)
        self.manage_button = Gtk.MenuButton(label='Manage Profile', tooltip_text='Manage the selected VPN profile')
        self.manage_button.update_property([Gtk.AccessibleProperty.LABEL], ['Manage Profile'])
        profiles.set_header_suffix(self.manage_button)
        popover = Gtk.Popover()
        menu_content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        margins(menu_content, 8)
        self.manage_button.set_popover(popover)
        popover.set_child(menu_content)
        def menu_action(callback):
            def activate(*args):
                popover.set_visible(False)
                callback(*args)
            return activate
        self.new_button = icon_button('New', 'list-add-symbolic', menu_action(self.new_profile))
        self.view_button = icon_button('View', 'dialog-information-symbolic', menu_action(self.view_profile))
        self.edit_button = icon_button('Edit', 'document-edit-symbolic', menu_action(self.edit_profile))
        self.delete_button = icon_button('Delete', 'user-trash-symbolic', menu_action(self.delete_profile))
        for widget in (self.new_button, self.view_button, self.edit_button, self.delete_button):
            widget.add_css_class('flat')
            widget.get_child().set_halign(Gtk.Align.START)
            menu_content.append(widget)
        self.connection_summary = Adw.PreferencesGroup(title='Active connection')
        self.connected_profile = Adw.ActionRow(title='Profile', subtitle_selectable=True)
        self.connected_server = Adw.ActionRow(title='Server', subtitle_selectable=True)
        for row, icon in ((self.connected_profile, 'network-vpn-symbolic'),
                          (self.connected_server, 'network-server-symbolic')):
            row.add_prefix(Gtk.Image(icon_name=icon, css_classes=['dim-label']))
        self.connection_summary.add(self.connected_profile)
        self.connection_summary.add(self.connected_server)
        content.append(self.connection_summary)
        self.operation_note = label('', 'dim-label')
        self.operation_note.set_xalign(.5)
        content.append(self.operation_note)

        credentials = Adw.PreferencesGroup(title='Sign in')
        self.credentials = credentials
        self.username = Adw.EntryRow(title='Username')
        credentials.add(self.username)
        self.remember = Adw.SwitchRow(title='Remember password', subtitle='Stored by FortiClient, never by this app')
        self.remember.set_active(self.preferences.data['remember_password'])
        credentials.add(self.remember)
        content.append(credentials)
        self.primary = button('Connect', self.primary_action, 'suggested-action')
        self.primary.set_size_request(-1, 42)
        content.append(self.primary)
        self.cancel_button = button('Cancel', lambda *_: self.cancel_connection())
        self.cancel_button.set_size_request(-1, 42)
        self.cancel_button.set_visible(False)
        content.append(self.cancel_button)
        self.notice_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10,
                                  css_classes=['card', 'operation-notice'], visible=False)
        notice_heading = Gtk.Box(spacing=8)
        self.notice_icon = Gtk.Image(icon_name='dialog-warning-symbolic')
        self.notice_title = label('', 'heading')
        notice_heading.append(self.notice_icon)
        notice_heading.append(self.notice_title)
        self.notice_box.append(notice_heading)
        self.error_label = label('')
        self.error_label.set_selectable(True)
        self.notice_box.append(self.error_label)
        self.warning_label = label('')
        self.warning_label.set_selectable(True)
        self.notice_box.append(self.warning_label)
        notice_actions = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        notice_actions.set_halign(Gtk.Align.START)
        self.retry_button = button('Try Again', self.retry_operation)
        self.retry_button.set_visible(False)
        notice_actions.append(self.retry_button)
        self.details_button = button('View Diagnostics', lambda *_: self.show_diagnostics(), 'flat')
        notice_actions.append(self.details_button)
        self.notice_box.append(notice_actions)
        content.insert_child_after(self.notice_box, heading)
        self.setup_help = button('Setup Help', lambda *_: self.show_setup())
        self.setup_help.set_halign(Gtk.Align.START)
        self.setup_help.set_visible(False)
        content.append(self.setup_help)
        self.note = label('Closing this window keeps an established VPN connection running.', 'dim-label')
        content.append(self.note)
        self.toast_overlay = Adw.ToastOverlay()
        self.toast_overlay.set_child(root)
        self.set_content(self.toast_overlay)
        self.connect('close-request', self.close_request)
        self.style_handler = Adw.StyleManager.get_default().connect('notify::high-contrast',
            lambda *_: self.render(self.workflow.state))
        self.animations_handler = Gtk.Settings.get_default().connect('notify::gtk-enable-animations',
            self.animation_preference_changed)
        self.prepared_editor = ProfileEditor(self, None)
        self.prepared_editor.get_child().measure(Gtk.Orientation.VERTICAL, 420)
        self.editor_prepare_source = None
        self.workflow.refresh()

    def selected(self):
        index = self.profile_row.get_selected()
        profiles = self.workflow.state.profiles
        return profiles[index] if index < len(profiles) else None

    def select_profile(self, *_):
        if self.rendering:
            return
        profile = self.selected()
        if profile:
            self.username.set_text(profile.username)
            self.remember.set_active(self.preferences.data['remember_password'])
            try:
                self.preferences.select(profile.name)
            except OSError:
                self.error_label.set_text('Could not save your last selected profile.')
        self.render(self.workflow.state)

    def render(self, state):
        old = self.selected()
        names = [p.name for p in state.profiles]
        if names != self.names:
            wanted = old.name if old else self.preferences.last_profile
            self.rendering = True
            self.profile_row.set_model(Gtk.StringList.new(names))
            self.profile_row.set_selected(names.index(wanted) if wanted in names else 0)
            self.names = names
            self.rendering = False
            profile = self.selected()
            self.username.set_text(profile.username if profile else '')
        if state.status.state != 'unavailable':
            self.cli_seen = True
        self.setup_group.set_visible(state.setup_required)
        self.profiles_group.set_visible(not state.setup_required)
        self.setup_help.set_visible(state.status.state == 'unavailable' and not state.setup_required)
        working = state.busy not in (None, 'refreshing')
        auth = state.busy == 'awaiting authentication'
        connected = state.status.state == 'connected'
        if connected != self.hero_connected:
            self.hero_connected = connected
            asset = APP_ID + ('-connected.svg' if connected else '.svg')
            self.hero_icon.set_from_file(str(Path(__file__).parent / 'assets' / asset))
            self.hero_icon.update_property([Gtk.AccessibleProperty.LABEL],
                                           ['Forti VPN connected' if connected else 'Forti VPN'])
        pending = state.status.state in ('connecting', 'awaiting authentication')
        unconfirmed = pending and not working
        failed_attempt = unconfirmed and state.error_origin == 'connecting' and bool(state.error)
        status = state.busy if working else state.status.state
        previous_status = self.status_label.get_text()
        titles = {'unavailable': 'FortiClient unavailable', 'disconnected': 'Disconnected',
                  'connecting': 'Connecting…', 'awaiting authentication': 'Awaiting authentication…',
                  'connected': 'Connected', 'disconnecting': 'Disconnecting…',
                  'saving profile': 'Saving profile…', 'removing profile': 'Removing profile…'}
        self.status_label.set_text(titles.get(status, status.capitalize()))
        if failed_attempt:
            self.status_label.set_text('Connection not confirmed')
        if connected and state.status.profile and not working:
            self.status_label.set_text('Connected · ' + state.status.profile)
        if self.status_label.get_text() != previous_status:
            self.status_badge.set_opacity(.85)
            self.fade(self.status_badge, 1, 'badge_animation', 180)
            self.status_label.announce(self.status_label.get_text(), Gtk.AccessibleAnnouncementPriority.MEDIUM)
        self.spinner.set_spinning(working)
        self.spinner.set_visible(working)
        idle = not state.busy
        profile = self.selected()
        locked = bool(profile and (profile.managed or (state.status.state != 'disconnected'
                      and (state.status.profile is None or state.status.profile == profile.name))))
        self.profile_row.set_sensitive(idle)
        self.refresh_button.set_sensitive(idle)
        self.new_button.set_sensitive(idle and state.status.state == 'disconnected')
        self.new_button.set_tooltip_text('New profile' if state.status.state == 'disconnected'
                                         else 'Disconnect the VPN before creating a profile.')
        self.view_button.set_sensitive(idle and profile is not None)
        self.edit_button.set_sensitive(idle and profile is not None and not locked)
        self.delete_button.set_sensitive(idle and profile is not None and not locked)
        lock_reason = ('This profile is managed by your organization.' if profile and profile.managed
                       else 'Disconnect this profile before editing or deleting it.' if locked else '')
        for widget, name in ((self.edit_button, 'Edit'), (self.delete_button, 'Delete')):
            widget.set_tooltip_text(lock_reason or name + ' profile')
            widget.update_property([Gtk.AccessibleProperty.DESCRIPTION], [lock_reason or name + ' profile'])
        self.manage_button.set_sensitive(idle)
        show_summary = connected and status not in ('connecting', 'awaiting authentication')
        self.connection_summary.set_visible(show_summary)
        active_profile = next((p for p in state.profiles if p.name == state.status.profile), None)
        self.connected_profile.set_subtitle(GLib.markup_escape_text(state.status.profile or 'Unknown profile'))
        self.connected_server.set_subtitle(GLib.markup_escape_text(active_profile.gateway if active_profile and active_profile.gateway
                                          else 'Server details unavailable'))
        self.credentials.set_visible(status == 'disconnected')
        self.username.set_sensitive(idle and state.status.state == 'disconnected')
        self.remember.set_sensitive(idle and state.status.state == 'disconnected')
        instructions = {
            'connecting': 'Contacting the VPN server. You may be asked to sign in.',
            'awaiting authentication': 'Complete the sign-in prompt to continue, or cancel this attempt.',
            'disconnecting': 'Disconnecting from the VPN. Please wait.',
            'unavailable': 'Check that FortiClient is installed and running, then refresh.',
        }
        self.operation_note.set_text(instructions.get(status, ''))
        self.operation_note.set_visible(status in instructions and not state.setup_required)
        if connected:
            primary_label = 'Disconnect'
        elif status == 'disconnecting':
            primary_label = 'Cancelling…'
        elif unconfirmed:
            primary_label = 'Check Status'
        else:
            primary_label = 'Connect'
        self.primary.set_label(primary_label)
        if connected:
            self.primary.remove_css_class('suggested-action')
            self.primary.add_css_class('destructive-action')
        else:
            self.primary.remove_css_class('destructive-action')
            self.primary.add_css_class('suggested-action')
        self.primary.set_sensitive(idle and (connected or unconfirmed or
                                             (profile is not None and state.status.state == 'disconnected')))
        cancellable = state.busy in ('connecting', 'awaiting authentication')
        self.cancel_button.set_label('Cancel Attempt' if unconfirmed else 'Cancel')
        self.cancel_button.set_sensitive(cancellable or (unconfirmed and idle))
        self.cancel_button.set_visible(cancellable or unconfirmed)
        self.primary.set_visible(not cancellable and not state.setup_required)
        if failed_attempt:
            self.operation_note.set_text('Check the current status or cancel the unfinished attempt.')
        previous_error = self.error_label.get_text()
        self.error_label.set_text(state.error)
        if state.error and state.error != previous_error:
            self.error_label.announce(state.error, Gtk.AccessibleAnnouncementPriority.HIGH)
        self.error_label.set_visible(bool(state.error) and not state.setup_required)
        warning_text = '\n\n'.join(w for w in state.warnings if w != state.error)
        previous_warning = self.warning_label.get_text()
        self.warning_label.set_text(warning_text)
        self.warning_label.set_visible(bool(warning_text))
        if warning_text and warning_text != previous_warning:
            self.warning_label.announce(warning_text, Gtk.AccessibleAnnouncementPriority.MEDIUM)
        self.notice_box.set_visible(bool(state.error or warning_text) and not state.setup_required)
        title = ('Login temporarily blocked' if 'temporarily blocked login' in state.error else
                 'Profile could not be loaded' if 'could not load this VPN profile' in state.error else
                 'Connection issue' if state.error_origin == 'connecting' else 'Operation failed')
        self.notice_title.set_text(title if state.error else 'Warning')
        for widget in (self.notice_icon, self.notice_title):
            widget.remove_css_class('error')
            widget.remove_css_class('warning')
            widget.add_css_class('error' if state.error else 'warning')

        retry = self.retry_choice(state)
        self.retry_button.set_visible(bool(state.error) and retry is not None and self.profile_dialog is None)
        self.retry_button.set_sensitive(idle)
        self.retry_button.set_label(retry or 'Try Again')
        badge_style = ('connected' if status == 'connected' else 'unavailable' if status == 'unavailable' or failed_attempt
                       else 'authentication' if status == 'awaiting authentication'
                       else 'idle' if status == 'disconnected' else 'progress')
        for style in ('connected', 'unavailable', 'authentication', 'idle', 'progress'):
            self.status_badge.remove_css_class('badge-' + style)
        self.status_badge.add_css_class('badge-' + badge_style)
        icons = {'connected': 'emblem-ok-symbolic', 'unavailable': 'dialog-warning-symbolic',
                 'authentication': 'dialog-password-symbolic', 'idle': 'network-offline-symbolic',
                 'progress': 'view-refresh-symbolic'}
        self.status_icon.set_from_icon_name(icons[badge_style])
        self.status_icon.set_visible(not working)
        high_contrast = Adw.StyleManager.get_default().get_high_contrast()
        if high_contrast:
            self.connection_header.add_css_class('high-contrast')
        else:
            self.connection_header.remove_css_class('high-contrast')
        for surface in (self.connection_header, self.connection_glow):
            if status == 'connected' and not high_contrast:
                surface.add_css_class('is-connected')
            else:
                surface.remove_css_class('is-connected')
        glow_visible = status == 'connected' and not high_contrast
        if high_contrast:
            self.glow_visible = False
            if self.glow_animation:
                self.glow_animation.pause()
            self.connection_glow.set_opacity(0)
        elif glow_visible != self.glow_visible:
            self.glow_visible = glow_visible
            self.fade(self.connection_glow, float(glow_visible), 'glow_animation', 240)
        indicator = getattr(self.get_application(), 'indicator', None)
        if indicator:
            indicator.update(self.status_label.get_text(), connected and idle and not self.exit_requested)
        self.note.set_visible(not state.setup_required)
        if self.setup_dialog:
            self.setup_dialog.update()
        if self.settings_window:
            self.settings_window.update()
        if self.exit_requested and not state.busy:
            GLib.idle_add(self.advance_exit)
        if self.challenge_window and not auth:
            self.challenge_window.close()
            self.challenge_window = None

    def primary_action(self, *_):
        state = self.workflow.state
        if state.busy:
            return
        if state.status.state == 'connected':
            self.workflow.disconnect()
        elif state.status.state == 'disconnected' and self.selected():
            self.workflow.connect(self.selected(), self.username.get_text(), self.remember.get_active())
        else:
            self.workflow.refresh()

    def cancel_connection(self):
        state = self.workflow.state
        if not state.busy and state.status.state in ('connecting', 'awaiting authentication'):
            self.workflow.disconnect()
        else:
            self.workflow.cancel()
        if self.challenge_window:
            self.challenge_window.close()
            self.challenge_window = None

    def fade(self, widget, opacity, attribute, duration):
        previous = getattr(self, attribute)
        if previous:
            previous.pause()
        target = Adw.CallbackAnimationTarget.new(widget.set_opacity)
        animation = Adw.TimedAnimation.new(widget, widget.get_opacity(), opacity, duration, target)
        setattr(self, attribute, animation)
        if Adw.StyleManager.get_default().get_high_contrast():
            animation.skip()
        else:
            animation.play()  # libadwaita respects gtk-enable-animations and unmapped widgets.

    def animation_preference_changed(self, settings, *_):
        if not settings.get_property('gtk-enable-animations'):
            for animation in (self.badge_animation, self.glow_animation):
                if animation:
                    animation.skip()

    def show_toast(self, title):
        if self.last_toast:
            self.last_toast.dismiss()
        self.last_toast = Adw.Toast(title=title, timeout=3)
        self.toast_overlay.add_toast(self.last_toast)

    def operation_finished(self, operation, success):
        if operation == 'saving profile' and self.profile_dialog:
            self.profile_dialog.finish_save(success)
        if success and operation in ('saving profile', 'removing profile'):
            self.show_toast('Profile saved' if operation == 'saving profile' else 'Profile deleted')

    def retry_choice(self, state):
        if state.setup_required:
            return 'Check Again'
        if 'temporarily blocked login' in state.error:
            return None
        if state.error_origin == 'connecting' and state.status.state == 'disconnected' and self.selected():
            return 'Try Again'
        if state.error_origin == 'disconnecting' and state.status.state == 'connected':
            return 'Retry Disconnect'
        if state.error_origin == 'refreshing' or state.status.state == 'unavailable':
            return 'Refresh'
        if state.error_origin in ('saving profile', 'removing profile'):
            return 'Manage Profiles'
        return None

    def retry_operation(self, *_):
        state = self.workflow.state
        if state.busy or self.exit_requested or not state.error:
            return
        choice = self.retry_choice(state)
        if choice == 'Try Again':
            self.primary_action()
        elif choice == 'Retry Disconnect':
            self.workflow.disconnect()
        elif choice in ('Refresh', 'Check Again'):
            self.workflow.refresh()
        elif choice == 'Manage Profiles':
            self.manage_button.popup()

    def show_challenge(self, challenge):
        if self.challenge_window:
            self.challenge_window.close()
        self.challenge_window = AuthenticationDialog(self, challenge)
        self.challenge_window.present(self)

    def dialog(self, title, width=420):
        dialog = ContentDialog(title, width)
        return dialog, dialog.body

    def view_profile(self, *_):
        profile = self.selected()
        if not profile:
            return
        win, body = self.dialog('Profile details')
        body.append(label(profile.name, 'title-2'))
        group = Adw.PreferencesGroup()
        for key, value in profile.fields.items():
            group.add(Adw.ActionRow(title=GLib.markup_escape_text(key), subtitle=GLib.markup_escape_text(value), subtitle_selectable=True))
        body.append(group)
        close = button('Close', lambda *_: win.close())
        win.set_actions(close)
        win.set_default_widget(close)
        win.set_focus(close)
        win.present(self)

    def new_profile(self, *_):
        self.profile_editor(None)

    def edit_profile(self, *_):
        if self.selected():
            self.profile_editor(self.selected())

    def prepare_editor(self):
        self.editor_prepare_source = None
        if not self.workflow.closed and self.prepared_editor is None:
            self.prepared_editor = ProfileEditor(self, None)
            self.prepared_editor.get_child().measure(Gtk.Orientation.VERTICAL, 420)
        return GLib.SOURCE_REMOVE

    def schedule_editor_preparation(self):
        if self.editor_prepare_source is None and not self.workflow.closed:
            # Wait for the native closing animation to finish before building the next form.
            self.editor_prepare_source = GLib.timeout_add(500, self.prepare_editor)

    def profile_editor(self, existing):
        if self.profile_dialog is None:
            self.profile_dialog = self.prepared_editor or ProfileEditor(self, None)
            self.prepared_editor = None
            self.profile_dialog.configure(existing)
        self.profile_dialog.present(self)

    def delete_profile(self, *_):
        profile = self.selected()
        if not profile:
            return
        dialog = Adw.AlertDialog(heading='Delete this profile?',
                                   body='Remove “' + profile.name + '” from FortiClient? This cannot be undone.')
        dialog.add_response('cancel', 'Cancel')
        dialog.add_response('delete', 'Delete')
        dialog.set_response_appearance('delete', Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response('cancel')
        dialog.set_close_response('cancel')
        dialog.connect('response', lambda _, response: self.workflow.remove(profile) if response == 'delete' else None)
        dialog.present(self)

    def apply_preferences(self):
        schemes = {'system': Adw.ColorScheme.DEFAULT, 'light': Adw.ColorScheme.FORCE_LIGHT,
                   'dark': Adw.ColorScheme.FORCE_DARK}
        Adw.StyleManager.get_default().set_color_scheme(schemes[self.preferences.data['appearance']])
        if hasattr(self, 'remember'):
            self.remember.set_active(self.preferences.data['remember_password'])
        if hasattr(self, 'note'):
            self.note.set_text('Close to keep running in the top bar.' if self.preferences.data['close_behavior'] == 'hide'
                               else 'Closing this window disconnects the VPN and quits.')

    def show_setup(self):
        self.present()
        if self.setup_dialog is None:
            self.setup_dialog = SetupDialog(self)
        self.setup_dialog.present(self)

    def show_settings(self):
        self.present()
        if self.settings_window is None:
            self.settings_window = SettingsDialog(self)
            def closed(*_):
                self.settings_window = None
                return False
            self.settings_window.connect('closed', closed)
        self.settings_window.present(self)

    def show_diagnostics(self):
        self.show_settings()
        self.settings_window.set_visible_page(self.settings_window.diagnostics_page)

    def indicator_changed(self, available):
        self.indicator_available = available
        if not available and not self.get_visible():
            self.present()
        if self.settings_window:
            self.settings_window.update()

    def close_request(self, *_):
        if self.exiting:
            return False
        if self.preferences.data['close_behavior'] == 'hide':
            if not self.indicator_available:
                self.workflow.state.error = 'Top-bar integration is unavailable. Use the menu to disconnect and quit, or enable your desktop’s AppIndicator extension.'
                self.render(self.workflow.state)
                return True
            if self.get_visible_dialog() or self.workflow.state.busy:
                self.workflow.state.error = 'Finish or cancel the current operation before hiding the window.'
                self.render(self.workflow.state)
                return True
            if self.settings_window:
                self.settings_window.close()
            self.set_visible(False)
            return True
        self.request_exit()
        return True

    def request_exit(self):
        if self.workflow.state.setup_required and not self.cli_seen:
            # A fresh setup screen has never had a usable CLI or started a connection.
            self.exiting = True
            self.get_application().quit()
            return
        if self.exit_requested:
            return
        self.exit_requested = True
        self.exit_checked = False
        self.present()
        self.cancel_connection()
        self.advance_exit()

    def advance_exit(self):
        if self.exiting or not self.exit_requested or self.workflow.state.busy:
            return GLib.SOURCE_REMOVE
        if not self.exit_checked:
            self.exit_checked = True
            self.workflow._start('checking before exit', lambda: None)
            return GLib.SOURCE_REMOVE
        state = self.workflow.state
        if state.status.state == 'disconnected':
            self.exiting = True
            self.get_application().quit()
        elif state.status.state == 'unavailable' or state.error_origin == 'disconnecting':
            self.exit_requested = False
            state.error = 'Cannot confirm VPN disconnection. The app remains open. Check FortiClient and retry Disconnect and Quit.'
            self.render(state)
        else:
            self.workflow.disconnect()
        return GLib.SOURCE_REMOVE

    def shutdown(self):
        self.workflow.close()
        if self.editor_prepare_source is not None:
            GLib.source_remove(self.editor_prepare_source)
        self.prepared_editor = None
        Adw.StyleManager.get_default().disconnect(self.style_handler)
        Gtk.Settings.get_default().disconnect(self.animations_handler)
        for dialog in list(self.get_dialogs()):
            dialog.force_close()
        for animation in (self.badge_animation, self.glow_animation):
            if animation:
                animation.pause()
        self.exiting = True
        self.destroy()


class Application(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)
        self.window = None
        self.indicator = None

    def do_startup(self):
        Adw.Application.do_startup(self)
        self.hold()
        Gtk.Window.set_default_icon_name(APP_ID)
        Gtk.IconTheme.get_for_display(Gdk.Display.get_default()).add_search_path(str(Path(__file__).parent / 'assets'))
        provider = Gtk.CssProvider()
        provider.load_from_path(str(Path(__file__).parent / 'assets' / 'style.css'))
        Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        for name, callback in [('settings', lambda: self.window.show_settings()), ('setup', lambda: self.window.show_setup()),
                               ('quit', lambda: self.window.request_exit())]:
            action = Gio.SimpleAction.new(name, None)
            action.connect('activate', lambda action, param, callback=callback: callback())
            self.add_action(action)
        self.set_accels_for_action('app.settings', ['<Primary>comma'])
        self.set_accels_for_action('app.quit', ['<Primary>q'])

    def do_activate(self):
        if self.window is None:
            self.window = UtilityWindow(self)
            self.window.apply_preferences()
            if not os.environ.get('FORTI_VPN_DISABLE_INDICATOR'):
                try:
                    icon_dir = str(Path(__file__).parent / 'assets')
                    self.indicator = Indicator(self.window.present, self.window.show_settings,
                                               self.window.workflow.disconnect, self.window.request_exit,
                                               self.window.indicator_changed, icon_dir)
                except GLib.Error:
                    self.window.indicator_changed(False)
        self.window.present()

    def do_shutdown(self):
        if self.indicator:
            self.indicator.close()
        if self.window:
            self.window.shutdown()
        Adw.Application.do_shutdown(self)


def main():
    if os.geteuid() == 0:
        raise SystemExit('Run Forti VPN as your regular desktop user, not with sudo.')
    return Application().run(None)
