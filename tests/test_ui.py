"""Opt-in native-widget checks; requires a graphical session."""
import os
from pathlib import Path
import time
import tempfile
import unittest
from unittest.mock import patch

if os.environ.get('RUN_GUI_TESTS') == '1':
    from forti_vpn.ui import Application
    from forti_vpn.backend import Profile, Status
    from forti_vpn.workflow import Challenge, Workflow
    from gi.repository import Adw, Gio, GLib, Gtk


def pump(duration=.15):
    end = time.monotonic() + duration
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        time.sleep(.005)


def descendants(widget):
    yield widget
    child = widget.get_first_child()
    while child:
        yield from descendants(child)
        child = child.get_next_sibling()


def press_escape(window):
    """Activate the native Escape shortcut installed on the mapped dialog sheet."""
    for widget in descendants(window):
        if not widget.get_mapped():
            continue
        for controller in widget.observe_controllers():
            if isinstance(controller, Gtk.ShortcutController):
                for shortcut in controller:
                    if shortcut.get_trigger().to_string() == 'Escape':
                        if shortcut.get_action().activate(Gtk.ShortcutActionFlags.EXCLUSIVE,
                                                          widget, shortcut.get_arguments()):
                            return True
    return False


@unittest.skipUnless(os.environ.get('RUN_GUI_TESTS') == '1', 'Set RUN_GUI_TESTS=1 in a desktop session')
class UITests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.environment = patch.dict(os.environ, {'FORTI_VPN_DISABLE_INDICATOR': '1', 'XDG_CONFIG_HOME': cls.temp.name})
        cls.environment.start()
        class FakeCLI:
            interactive_timeout = 120
            current = Status('disconnected')
            def disconnect(self):
                self.current = Status('disconnected')
                return self.current
            def status(self):
                return self.current
            def list_profiles(self):
                return [Profile('Office VPN')]
            def view(self, profile):
                return Profile(profile.name, {'Remote Gateway': 'localhost:443', 'Username': 'worker'})
        cls.patcher = patch('forti_vpn.ui.Workflow', side_effect=lambda *args, **kwargs: Workflow(*args, cli=FakeCLI(), **kwargs))
        cls.patcher.start()
        cls.app = Application()
        cls.app.set_application_id('io.github.fortivpn.Utility.Tests')
        cls.app.set_flags(Gio.ApplicationFlags.NON_UNIQUE)
        cls.app.register(None)
        cls.app.activate()
        pump(.5)
        cls.window = cls.app.get_active_window()

    @classmethod
    def tearDownClass(cls):
        cls.window.shutdown()
        pump()
        cls.patcher.stop()
        cls.app.quit()
        cls.environment.stop()
        cls.temp.cleanup()

    def tearDown(self):
        for dialog in list(self.window.get_dialogs()):
            dialog.force_close()
        pump(.3)
        for win in list(Gtk.Window.get_toplevels()):
            if win is not self.window:
                win.close()
        self.window.challenge_window = None
        self.window.workflow.state.busy = None
        self.window.workflow.state.status = Status('disconnected')
        self.window.workflow.state.error = ''
        self.window.workflow.state.error_origin = ''
        self.window.workflow.state.warnings.clear()
        self.window.workflow.state.setup_required = False
        self.window.cli_seen = True
        self.window.exit_requested = False
        self.window.exiting = False
        self.window.workflow.cli.current = Status('disconnected')
        self.window.preferences.set('close_behavior', 'hide')
        self.window.indicator_available = False
        self.window.present()
        self.window.render(self.window.workflow.state)
        pump()

    def test_close_hides_and_preserves_connection(self):
        self.window.indicator_available = True
        self.window.workflow.cli.current = Status('connected', 'Office VPN')
        self.window.workflow.state.status = Status('connected', 'Office VPN')
        self.assertTrue(self.window.close_request())
        self.assertFalse(self.window.get_visible())
        self.assertEqual(self.window.workflow.cli.current.state, 'connected')
        self.window.indicator_changed(False)
        self.assertTrue(self.window.get_visible())

    def test_no_indicator_never_hides_window(self):
        self.window.indicator_available = False
        self.assertTrue(self.window.close_request())
        self.assertTrue(self.window.get_visible())

    def test_quit_disconnects_and_checks_status(self):
        self.window.workflow.cli.current = Status('connected', 'Office VPN')
        self.window.workflow.state.status = Status('disconnected')  # stale initial state
        with patch.object(self.app, 'quit') as quit_app:
            self.window.request_exit()
            pump(.5)
            self.assertEqual(self.window.workflow.cli.current.state, 'disconnected')
            quit_app.assert_called_once()

    def test_failed_disconnect_keeps_app_open(self):
        from forti_vpn.backend import VPNError
        self.window.workflow.cli.current = Status('connected', 'Office VPN')
        with patch.object(self.window.workflow.cli, 'disconnect', side_effect=VPNError('failure')):
            with patch.object(self.app, 'quit') as quit_app:
                self.window.request_exit()
                pump(.5)
                quit_app.assert_not_called()
                self.assertFalse(self.window.exit_requested)
                self.assertTrue(self.window.get_visible())

    def test_settings_and_diagnostics(self):
        self.window.show_settings()
        settings = self.window.settings_window
        settings.close_behavior.set_selected(1)
        settings.appearance.set_selected(2)
        settings.remember.set_active(True)
        self.assertEqual(self.window.preferences.data['close_behavior'], 'quit')
        self.assertEqual(self.window.preferences.data['appearance'], 'dark')
        self.assertTrue(self.window.preferences.data['remember_password'])
        self.window.workflow.state.diagnostics[:] = ['Sanitized event']
        settings.update()
        buffer = settings.text.get_buffer()
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), 'Sanitized event')
        with patch.object(settings, 'get_clipboard') as clipboard:
            settings.copy()
            clipboard.return_value.set.assert_called_once_with('Sanitized event')
        settings.clear()
        self.assertEqual(self.window.workflow.state.diagnostics, [])
        settings.appearance.set_selected(0)

    def test_badge_and_glow_follow_verified_status(self):
        state = self.window.workflow.state
        state.status = Status('connected', 'Office VPN')
        self.window.render(state)
        self.assertTrue(self.window.status_badge.has_css_class('badge-connected'))
        self.assertTrue(self.window.connection_glow.has_css_class('is-connected'))
        state.busy = 'disconnecting'
        self.window.render(state)
        self.assertFalse(self.window.connection_glow.has_css_class('is-connected'))
        state.busy = 'awaiting authentication'
        self.window.render(state)
        self.assertTrue(self.window.status_badge.has_css_class('badge-authentication'))
        self.assertFalse(self.window.primary.get_visible())
        self.assertTrue(self.window.cancel_button.get_visible())
        state.busy = None
        state.status = Status('disconnected')
        self.window.render(state)
        self.assertTrue(self.window.status_badge.has_css_class('badge-idle'))
        self.assertFalse(self.window.connection_glow.has_css_class('is-connected'))
        self.assertTrue(self.window.primary.get_visible())

    def test_high_contrast_disables_decorative_glow(self):
        self.window.workflow.state.status = Status('connected', 'Office VPN')
        with patch.object(Adw.StyleManager.get_default(), 'get_high_contrast', return_value=True):
            self.window.render(self.window.workflow.state)
            self.assertFalse(self.window.connection_glow.has_css_class('is-connected'))
            self.assertTrue(self.window.connection_header.has_css_class('high-contrast'))
            self.assertIn('Connected', self.window.status_label.get_text())

    def test_main_window_and_actions(self):
        self.assertEqual(self.window.status_label.get_text(), 'Disconnected')
        self.assertEqual(self.window.selected().name, 'Office VPN')
        self.assertTrue(self.window.primary.get_sensitive())
        self.window.primary.grab_focus()
        pump()
        self.assertIsNotNone(self.window.get_focus())
        state = self.window.workflow.state
        state.status = Status('connected', 'Office VPN')
        self.window.render(state)
        self.assertFalse(self.window.edit_button.get_sensitive())
        self.assertFalse(self.window.delete_button.get_sensitive())
        self.assertEqual(self.window.primary.get_label(), 'Disconnect')
        state.status = Status('disconnected')
        self.window.render(state)

    def test_connection_layout_preserves_inputs_and_uses_actual_profile(self):
        state = self.window.workflow.state
        state.profiles.append(Profile('Other VPN', {'Remote Gateway': 'vpn.example.org:443'}))
        self.window.render(state)
        self.window.username.set_text('edited username')
        self.window.remember.set_active(True)
        state.busy = 'connecting'
        self.window.render(state)
        self.assertFalse(self.window.credentials.get_visible())
        self.assertFalse(self.window.connection_summary.get_visible())
        self.assertTrue(self.window.operation_note.get_visible())
        self.assertTrue(self.window.cancel_button.get_sensitive())
        self.assertFalse(self.window.manage_button.get_sensitive())
        state.busy = None
        state.status = Status('connected', 'Other VPN')
        self.window.render(state)
        self.assertTrue(self.window.connection_summary.get_visible())
        self.assertFalse(self.window.credentials.get_visible())
        self.assertEqual(self.window.connected_profile.get_subtitle(), 'Other VPN')
        self.assertEqual(self.window.connected_server.get_subtitle(), 'vpn.example.org:443')
        self.assertFalse(self.window.primary.has_css_class('suggested-action'))
        self.assertTrue(self.window.primary.has_css_class('destructive-action'))
        state.busy = 'disconnecting'
        self.window.render(state)
        self.assertFalse(self.window.primary.get_sensitive())
        self.assertEqual(self.window.primary.get_label(), 'Disconnect')
        self.assertFalse(self.window.cancel_button.get_visible())
        state.busy = None
        state.status = Status('disconnected')
        self.window.render(state)
        self.assertTrue(self.window.credentials.get_visible())
        self.assertFalse(self.window.connection_summary.get_visible())
        self.assertTrue(self.window.primary.has_css_class('suggested-action'))
        self.assertFalse(self.window.primary.has_css_class('destructive-action'))
        self.assertEqual(self.window.username.get_text(), 'edited username')
        self.assertTrue(self.window.remember.get_active())
        state.profiles.pop()
        self.window.render(state)

    def test_error_and_warning_notices_are_visible_and_open_diagnostics(self):
        state = self.window.workflow.state
        state.error = 'Too many bad login attempts. The VPN server temporarily blocked login. Wait a few minutes.'
        state.error_origin = 'connecting'
        self.window.render(state)
        self.assertTrue(self.window.notice_box.get_visible())
        self.assertEqual(self.window.notice_title.get_text(), 'Login temporarily blocked')
        self.assertIn('Too many bad login attempts', self.window.error_label.get_text())
        self.window.details_button.emit('clicked')
        self.assertIs(self.window.settings_window.get_visible_page(), self.window.settings_window.diagnostics_page)
        state.error = ''
        state.status = Status('connected', 'Office VPN')
        state.warnings = ['Your password could not be saved.']
        self.window.render(state)
        self.assertEqual(self.window.notice_title.get_text(), 'Warning')
        self.assertTrue(self.window.warning_label.get_visible())
        self.assertEqual(self.window.status_label.get_text(), 'Connected · Office VPN')

    def test_rejected_connection_with_pending_cli_status_offers_check_and_cancel(self):
        from forti_vpn.backend import VPNError
        workflow = self.window.workflow
        workflow.cli.current = Status('connecting', 'Office VPN')
        with patch.object(workflow.cli, 'connect', create=True,
                          side_effect=VPNError('FortiClient rejected the operation.')):
            workflow.connect(Profile('Office VPN'), '', False)
            pump(.3)
        self.assertEqual(workflow.state.status.state, 'connecting')
        self.assertEqual(self.window.status_label.get_text(), 'Connection not confirmed')
        self.assertEqual(self.window.primary.get_label(), 'Check Status')
        self.assertFalse(self.window.primary.has_css_class('destructive-action'))
        self.assertEqual(self.window.cancel_button.get_label(), 'Cancel Attempt')
        self.assertTrue(self.window.cancel_button.get_visible())
        with patch.object(workflow, 'refresh') as refresh, patch.object(workflow, 'disconnect') as disconnect:
            self.window.primary.emit('clicked')
            refresh.assert_called_once()
            disconnect.assert_not_called()
            self.window.cancel_button.emit('clicked')
            disconnect.assert_called_once()

    def test_profile_menu_lock_explanation_and_keyboard_focus(self):
        state = self.window.workflow.state
        state.status = Status('connected', 'Office VPN')
        self.window.render(state)
        self.assertTrue(self.window.view_button.get_sensitive())
        self.assertFalse(self.window.edit_button.get_sensitive())
        self.assertFalse(self.window.delete_button.get_sensitive())
        self.assertIn('Disconnect', self.window.edit_button.get_tooltip_text())
        self.window.manage_button.popup()
        pump()
        self.window.view_button.grab_focus()
        self.assertTrue(self.window.view_button.has_focus())
        self.window.view_button.emit('clicked')
        pump()
        self.assertFalse(self.window.manage_button.get_popover().get_visible())
        self.assertIn('Profile details', [dialog.get_title() for dialog in self.window.get_dialogs()])
        state.status = Status('disconnected')
        state.profiles[0] = Profile('Office VPN', managed=True)
        self.window.render(state)
        self.assertIn('organization', self.window.delete_button.get_tooltip_text())
        self.assertFalse(self.window.edit_button.get_sensitive())
        state.profiles[0] = Profile('Office VPN', {'Remote Gateway': 'localhost:443', 'Username': 'worker'})

    def test_prepared_editor_populates_latest_profile_and_replenishes_after_close(self):
        self.window.prepare_editor()
        prepared = self.window.prepared_editor
        profile = Profile('Updated VPN', {'Remote Gateway': 'updated.example.org', 'Username': 'updated user'})
        self.window.profile_editor(profile)
        self.assertIs(self.window.profile_dialog, prepared)
        self.assertEqual(prepared.gateway.get_text(), 'updated.example.org')
        self.assertEqual(prepared.username.get_text(), 'updated user')
        prepared.close()
        pump(.7)
        self.assertIsNotNone(self.window.prepared_editor)
        self.assertIsNot(self.window.prepared_editor, prepared)
        self.window.profile_editor(None)
        self.assertEqual(self.window.profile_dialog.name.get_text(), '')
        self.assertEqual(self.window.profile_dialog.gateway.get_text(), '')

    def test_profile_forms_and_details_construct(self):
        for profile, title in ((None, 'New profile'), (self.window.selected(), 'Edit profile')):
            self.window.profile_editor(profile)
            pump()
            dialog = self.window.profile_dialog
            self.assertEqual(dialog.get_title(), title)
            self.assertIsNotNone(dialog.get_focus())
            self.assertIs(dialog.get_default_widget(), dialog.save_button)
            dialog.close()
            pump(.3)
        self.window.view_profile()
        pump()
        self.assertEqual(self.window.get_visible_dialog().get_title(), 'Profile details')

    def test_authentication_dialog_clears_secret_and_has_focus(self):
        self.window.workflow.state.busy = 'awaiting authentication'
        challenge = Challenge('mfa')
        self.window.show_challenge(challenge)
        pump()
        win = self.window.challenge_window
        self.assertIsNotNone(win.get_focus())
        entry = next(w for w in descendants(win) if isinstance(w, Adw.PasswordEntryRow))
        self.assertEqual(entry.get_title(), 'Verification code')
        entry.set_text('123456')
        proceed = next(w for w in descendants(win) if isinstance(w, Gtk.Button) and w.get_label() == 'Continue')
        proceed.emit('clicked')
        pump()
        self.assertEqual(challenge.responses.get_nowait(), '123456')
        self.assertEqual(entry.get_text(), '')

    def test_cancel_authentication_dialog(self):
        self.window.workflow.state.busy = 'awaiting authentication'
        challenge = Challenge('password')
        self.window.show_challenge(challenge)
        pump()
        self.window.challenge_window.close()
        pump()
        self.assertIsNone(challenge.responses.get_nowait())

    def test_enter_and_escape_authentication_clear_credentials(self):
        state = self.window.workflow.state
        state.busy = 'awaiting authentication'
        challenge = Challenge('password')
        self.window.show_challenge(challenge)
        pump()
        dialog = self.window.challenge_window
        self.assertIs(dialog.get_default_widget(), dialog.proceed)
        self.assertFalse(dialog.proceed.get_sensitive())
        dialog.entry.set_text('temporary secret')
        dialog.entry.emit('entry-activated')
        pump(.3)
        self.assertEqual(challenge.responses.get_nowait(), 'temporary secret')
        self.assertTrue(challenge.responses.empty())
        self.assertEqual(dialog.entry.get_text(), '')
        self.assertIsNone(self.window.challenge_window)
        challenge = Challenge('mfa')
        self.window.show_challenge(challenge)
        pump()
        dialog = self.window.challenge_window
        dialog.entry.set_text('123456')
        self.assertTrue(press_escape(self.window))
        pump(.3)
        self.assertIsNone(challenge.responses.get_nowait())
        self.assertEqual(dialog.entry.get_text(), '')
        self.assertIsNone(self.window.challenge_window)

    def test_profile_validation_and_failed_save_preserve_form(self):
        from forti_vpn.backend import VPNError
        self.window.profile_editor(self.window.selected())
        dialog = self.window.profile_dialog
        pump()
        dialog.gateway.set_text('')
        dialog.gateway.emit('entry-activated')
        self.assertTrue(dialog.gateway.has_css_class('error'))
        self.assertIn('gateway', dialog.error.get_text())
        self.assertIsNotNone(dialog.get_focus())
        dialog.gateway.set_text('vpn.example.org:443')
        with patch.object(self.window.workflow.cli, 'save_profile', create=True, side_effect=VPNError('Save rejected. Check the gateway.')):
            with patch.object(self.window, 'show_toast') as toast:
                dialog.gateway.emit('entry-activated')
                pump(.4)
                self.assertIs(self.window.profile_dialog, dialog)
                self.assertTrue(dialog.get_can_close())
                self.assertEqual(dialog.gateway.get_text(), 'vpn.example.org')
                self.assertEqual(dialog.port.get_text(), '443')
                self.assertEqual(dialog.save_button.get_label(), 'Retry Save')
                self.assertIn('Save rejected', dialog.error.get_text())
                toast.assert_not_called()
        with patch.object(self.window.workflow.cli, 'save_profile', create=True):
            with patch.object(self.window, 'show_toast') as toast:
                dialog.save_button.emit('clicked')
                pump(.4)
                self.assertIsNone(self.window.profile_dialog)
                toast.assert_called_once_with('Profile saved')
                self.window.workflow.refresh()
                pump(.3)
                toast.assert_called_once()

    def test_profile_port_and_certificate_picker_submission(self):
        self.window.profile_editor(self.window.selected())
        dialog = self.window.profile_dialog
        pump()
        self.assertEqual(dialog.gateway.get_text(), 'localhost')
        self.assertEqual(dialog.port.get_text(), '443')
        dialog.port.set_text('70000')
        dialog.save()
        self.assertTrue(dialog.port.has_css_class('error'))
        dialog.port.set_text('8443')
        dialog.cert.set_selected(1)
        with patch('forti_vpn.dialogs.Gtk.FileDialog') as factory:
            picker = factory.return_value
            picker.open_finish.return_value = Gio.File.new_for_path('/tmp/client certificate.p12')
            dialog.browse.emit('clicked')
            callback = picker.open.call_args.args[2]
            callback(picker, object())
        self.assertEqual(dialog.path.get_text(), '/tmp/client certificate.p12')
        with patch.object(self.window.workflow, 'save') as save:
            dialog.save()
            settings, existing = save.call_args.args
            self.assertEqual(settings.gateway, 'localhost:8443')
            self.assertEqual(settings.certificate, '/tmp/client certificate.p12')
            self.assertEqual(settings.authentication, '')
            self.assertEqual(existing.name, 'Office VPN')
        dialog.finish_save(False)

    def test_escape_cancels_save_and_leaves_form_for_retry(self):
        from forti_vpn.backend import Cancelled
        def saving(settings, existing, cancel, ask, log):
            ask('certificate_password')
            raise Cancelled('Profile save cancelled.')
        with patch.object(self.window.workflow.cli, 'save_profile', create=True, side_effect=saving):
            self.window.profile_editor(self.window.selected())
            dialog = self.window.profile_dialog
            dialog.gateway.set_text('vpn.example.org')
            dialog.save_button.emit('clicked')
            pump(.3)
            self.assertIsNotNone(self.window.challenge_window)
            auth = self.window.challenge_window
            auth.entry.set_text('temporary certificate password')
            self.assertTrue(press_escape(self.window))
            pump(.4)
            self.assertEqual(auth.entry.get_text(), '')
            self.assertEqual(dialog.gateway.get_text(), 'vpn.example.org')
            self.assertFalse(dialog.saving)
            self.assertIn('cancelled', dialog.error.get_text())
            self.assertTrue(press_escape(self.window))
            pump(.3)
            self.assertIsNone(self.window.profile_dialog)

    def test_escape_cancels_pending_save_before_closing_form(self):
        from forti_vpn.backend import Cancelled
        def saving(settings, existing, cancel, ask, log):
            cancel.wait(1)
            raise Cancelled('Profile save cancelled.')
        with patch.object(self.window.workflow.cli, 'save_profile', create=True, side_effect=saving):
            self.window.profile_editor(self.window.selected())
            dialog = self.window.profile_dialog
            dialog.save_button.emit('clicked')
            pump()
            self.assertFalse(dialog.get_can_close())
            self.assertTrue(press_escape(self.window))
            pump(.4)
            self.assertTrue(self.window.workflow.cancel_event.is_set())
            self.assertIs(self.window.profile_dialog, dialog)
            self.assertTrue(dialog.get_can_close())
            self.assertFalse(dialog.saving)

    def test_narrow_large_text_form_keeps_actions_and_error_visible(self):
        settings = Gtk.Settings.get_default()
        old_font = settings.get_property('gtk-font-name')
        try:
            settings.set_property('gtk-font-name', 'Sans 16')
            self.window.set_default_size(360, 650)
            self.window.profile_editor(None)
            dialog = self.window.profile_dialog
            pump(.3)
            dialog.save_button.emit('clicked')
            pump(.3)
            for widget in (dialog.cancel, dialog.save_button, dialog.error):
                success, bounds = widget.compute_bounds(self.window)
                self.assertTrue(success)
                self.assertGreaterEqual(bounds.get_y(), 0)
                self.assertLessEqual(bounds.get_y() + bounds.get_height(), self.window.get_height())
                self.assertTrue(widget.get_mapped())
        finally:
            self.window.profile_dialog.force_close()
            pump(.3)
            settings.set_property('gtk-font-name', old_font)
            self.window.set_default_size(480, 740)

    def test_retry_follows_actual_status_and_never_replays_delete(self):
        state = self.window.workflow.state
        state.error = 'Operation failed.'
        state.error_origin = 'connecting'
        state.status = Status('disconnected')
        self.window.render(state)
        self.assertEqual(self.window.retry_button.get_label(), 'Try Again')
        with patch.object(self.window.workflow, 'connect') as connect:
            self.window.retry_button.emit('clicked')
            connect.assert_called_once_with(self.window.selected(), self.window.username.get_text(), self.window.remember.get_active())
        state.status = Status('connected', 'Office VPN')
        self.window.render(state)
        self.assertFalse(self.window.retry_button.get_visible())
        state.error_origin = 'disconnecting'
        self.window.render(state)
        with patch.object(self.window.workflow, 'disconnect') as disconnect:
            self.window.retry_button.emit('clicked')
            disconnect.assert_called_once()
        state.error_origin = 'removing profile'
        self.window.render(state)
        self.assertEqual(self.window.retry_button.get_label(), 'Manage Profiles')
        with patch.object(self.window.workflow, 'remove') as remove:
            self.window.retry_button.emit('clicked')
            remove.assert_not_called()
        self.window.manage_button.popdown()
        state.status = Status('unavailable')
        self.window.render(state)
        with patch.object(self.window.workflow, 'refresh') as refresh:
            self.window.retry_button.emit('clicked')
            refresh.assert_called_once()

    def test_reduced_motion_finishes_fades_and_repeated_refresh_does_not_animate(self):
        settings = Gtk.Settings.get_default()
        enabled = settings.get_property('gtk-enable-animations')
        try:
            settings.set_property('gtk-enable-animations', False)
            state = self.window.workflow.state
            state.status = Status('connected', 'Office VPN')
            self.window.render(state)
            self.assertEqual(self.window.connection_glow.get_opacity(), 1)
            self.assertEqual(self.window.status_badge.get_opacity(), 1)
            previous = self.window.glow_animation
            self.window.render(state)
            self.assertIs(self.window.glow_animation, previous)
            state.status = Status('disconnected')
            self.window.render(state)
            self.assertEqual(self.window.connection_glow.get_opacity(), 0)
            self.assertEqual(self.window.status_badge.get_opacity(), 1)
        finally:
            settings.set_property('gtk-enable-animations', enabled)

    def test_settings_dialog_escape_and_diagnostics_feedback(self):
        self.window.show_settings()
        settings = self.window.settings_window
        settings.set_visible_page(self.window.settings_window.diagnostics_page)
        pump()
        with patch.object(settings, 'add_toast') as toast:
            settings.copy()
            self.assertEqual(toast.call_args.args[0].get_title(), 'Diagnostics copied')
        self.assertTrue(press_escape(self.window))
        pump(.3)
        self.assertIsNone(self.window.settings_window)

    def test_refresh_only_on_launch_and_manual_request(self):
        from forti_vpn.ui import UtilityWindow
        with patch.object(Workflow, 'refresh') as refresh:
            window = UtilityWindow(self.app)
            try:
                refresh.assert_called_once()
                refresh.reset_mock()
                window.present()
                window.notify('is-active')
                pump(3.5)
                refresh.assert_not_called()
                window.refresh_button.emit('clicked')
                refresh.assert_called_once()
            finally:
                window.shutdown()

    def test_missing_cli_setup_screen_and_check_again_recovery(self):
        from forti_vpn.backend import CLINotFound
        state = self.window.workflow.state
        with patch.object(self.window.workflow.cli, 'status', side_effect=CLINotFound('CLI missing')):
            self.window.workflow.refresh()
            pump(.3)
            self.window.show_setup()
            pump(.3)
        self.assertTrue(state.setup_required)
        self.assertTrue(self.window.setup_group.get_visible())
        self.assertFalse(self.window.profiles_group.get_visible())
        self.assertFalse(self.window.primary.get_visible())
        dialog = self.window.setup_dialog
        self.assertIn('not been found', dialog.check_status.get_text())
        self.assertEqual(dialog.pages.get_visible_child_name(), 'install')
        self.assertFalse(dialog.command_panel.get_visible())
        visible, installer_bounds = dialog.select_button.compute_bounds(self.window)
        _, footer_bounds = dialog.footer.compute_bounds(self.window)
        self.assertTrue(visible)
        self.assertLessEqual(installer_bounds.get_y() + installer_bounds.get_height(), footer_bounds.get_y())
        dialog.check.emit('clicked')
        pump(.3)
        self.assertFalse(state.setup_required)
        self.assertTrue(self.window.primary.get_visible())
        self.assertIn('ready', dialog.check_status.get_text())
        self.assertEqual(dialog.pages.get_visible_child_name(), 'ready')
        self.assertFalse(dialog.check.get_visible())

    def test_setup_checks_once_on_open_then_only_on_request(self):
        with patch.object(self.window.workflow, 'refresh', wraps=self.window.workflow.refresh) as refresh:
            self.window.show_setup()
            pump(.3)
            self.assertEqual(refresh.call_count, 1)
            self.window.show_setup()
            pump(2.2)
            self.assertEqual(refresh.call_count, 1)
            self.window.setup_dialog.show_help()
            self.window.setup_dialog.check.emit('clicked')
            pump(.3)
            self.assertEqual(refresh.call_count, 2)

    def test_installed_but_unavailable_client_shows_recovery(self):
        from forti_vpn.backend import VPNError
        with patch.object(self.window.workflow.cli, 'status', side_effect=VPNError('Service is unavailable')):
            self.window.show_setup()
            pump(.3)
        dialog = self.window.setup_dialog
        self.assertFalse(self.window.workflow.state.setup_required)
        self.assertEqual(dialog.pages.get_visible_child_name(), 'recovery')
        self.assertIn('CLI was found', dialog.recovery_details.get_text())
        dialog.check.emit('clicked')
        pump(.3)
        self.assertEqual(dialog.pages.get_visible_child_name(), 'ready')

    def test_setup_command_copy_and_fresh_setup_can_quit(self):
        self.window.show_setup()
        dialog = self.window.setup_dialog
        self.assertEqual(dialog.pages.get_visible_child_name(), 'ready')
        dialog.show_help()
        self.assertEqual(dialog.pages.get_visible_child_name(), 'install')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'FortiClient installer.deb'
            path.touch()
            dialog.set_package(str(path))
            self.assertTrue(dialog.command_panel.get_visible())
            with patch.object(dialog, 'get_clipboard') as clipboard:
                dialog.copy_button.emit('clicked')
                self.assertIn(str(path), clipboard.return_value.set.call_args.args[0])
                self.assertEqual(dialog.copy_button.get_label(), 'Command Copied')
        state = self.window.workflow.state
        state.status = Status('unavailable')
        state.setup_required = True
        self.window.cli_seen = False
        with patch.object(self.app, 'quit') as quit_app, patch.object(self.window.workflow, 'disconnect') as disconnect:
            self.window.request_exit()
            quit_app.assert_called_once()
            disconnect.assert_not_called()

    def test_setup_large_text_and_long_package_path_fit_narrow_window(self):
        settings = Gtk.Settings.get_default()
        font = settings.get_property('gtk-font-name')
        try:
            settings.set_property('gtk-font-name', 'Sans 16')
            self.window.set_default_size(360, 650)
            self.window.show_setup()
            dialog = self.window.setup_dialog
            with tempfile.TemporaryDirectory() as directory:
                package = Path(directory) / ('forticlient_' + 'x' * 140 + '.deb')
                package.touch()
                dialog.set_package(str(package))
                pump(.3)
                self.assertLessEqual(dialog.get_width(), self.window.get_width())
                self.assertLessEqual(dialog.get_child().measure(Gtk.Orientation.HORIZONTAL, -1)[0], self.window.get_width())
                success, bounds = dialog.check.compute_bounds(self.window)
                self.assertTrue(success)
                self.assertLessEqual(bounds.get_y() + bounds.get_height(), self.window.get_height())
                self.assertGreaterEqual(bounds.get_x(), 0)
        finally:
            self.window.setup_dialog.close()
            pump(.3)
            settings.set_property('gtk-font-name', font)
            self.window.set_default_size(480, 740)

    def test_themes_narrow_window_large_text_render(self):
        manager = Adw.StyleManager.get_default()
        old_scheme = manager.get_color_scheme()
        settings = Gtk.Settings.get_default()
        old_font = settings.get_property('gtk-font-name')
        output = Path('.cache/ui-previews')
        output.mkdir(parents=True, exist_ok=True)
        try:
            settings.set_property('gtk-font-name', 'Sans 16')
            self.window.set_default_size(360, 650)
            for name, scheme in [('light', Adw.ColorScheme.FORCE_LIGHT), ('dark', Adw.ColorScheme.FORCE_DARK)]:
                manager.set_color_scheme(scheme)
                pump(.3)
                paintable = Gtk.WidgetPaintable.new(self.window)
                snapshot = Gtk.Snapshot.new()
                paintable.snapshot(snapshot, self.window.get_width(), self.window.get_height())
                texture = self.window.get_renderer().render_texture(snapshot.to_node(), None)
                texture.save_to_png(str(output / (name + '.png')))
                self.assertGreater(texture.get_width(), 0)
                self.window.show_settings()
                self.assertEqual(self.window.settings_window.text.get_accessible_role(), Gtk.AccessibleRole.TEXT_BOX)
        finally:
            settings.set_property('gtk-font-name', old_font)
            manager.set_color_scheme(old_scheme)
            self.window.set_default_size(480, 740)
            pump()


if __name__ == '__main__':
    unittest.main()
