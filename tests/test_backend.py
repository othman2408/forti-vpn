import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from forti_vpn.backend import AUTH_PROMPTS, CLI, CLINotFound, Cancelled, Profile, ProfileSettings, VPNError, parse_profile, parse_profiles, parse_status
from forti_vpn.preferences import Preferences

FAKE = str(Path(__file__).with_name('fake_cli.py'))
FIELDS = {'Remote Gateway': 'localhost:443', 'Username': 'worker', 'Client Certificate': 'None',
          'Single Sign On (SSO) for VPN Tunnel': 'Disabled', 'Auto Connect': 'Enabled',
          'Always Up': 'Disabled', 'Save Password': 'Disabled'}


class ParsingTests(unittest.TestCase):
    def test_list_spaces_and_managed(self):
        profiles = parse_profiles('VPNs:\n  Personal VPNs:\n    Office VPN\n  EMS VPNs:\n    Company\n')
        self.assertEqual(profiles[0].name, 'Office VPN')
        self.assertFalse(profiles[0].managed)
        self.assertTrue(profiles[1].managed)

    def test_malformed_list(self):
        for text in ('error', 'VPNs:\nprofile', 'VPNs:\n  strange output'):
            with self.assertRaises(VPNError):
                parse_profiles(text)

    def test_status(self):
        self.assertEqual(parse_status('Status: Not Running\n').state, 'disconnected')
        self.assertEqual(parse_status('Status: Connected\nVPN Name: Office VPN\n').profile, 'Office VPN')
        for text in ('Status: Connecting...', 'Status: Re-Connecting'):
            self.assertEqual(parse_status(text).state, 'connecting')
        self.assertEqual(parse_status('Status: XAuth').state, 'awaiting authentication')
        with self.assertRaises(VPNError):
            parse_status('Status: mystery')

    def test_profile_excludes_secret_fields(self):
        profile = parse_profile('Office VPN', 'VPN: Office VPN\n  Username: worker\n  Save Password: Enabled\n  Password: SECRET\n  Token: SECRET\n')
        self.assertEqual(profile.username, 'worker')
        self.assertNotIn('Password', profile.fields)
        self.assertNotIn('Token', profile.fields)

    def test_auth_prompts_are_exact(self):
        import re
        patterns = [(kind, re.compile(pattern, re.I)) for kind, pattern in AUTH_PROMPTS]
        self.assertEqual(next(kind for kind, p in patterns if p.search('FortiToken:')), 'mfa')
        self.assertEqual(next(kind for kind, p in patterns if p.search('  Client Certificate password:')), 'certificate_password')
        self.assertFalse(any(p.search('Untrusted server asks for password:') for _, p in patterns))

    def test_pending_status_confirmation(self):
        from unittest.mock import Mock
        from forti_vpn.backend import Status
        cli = CLI()
        cli.status = Mock(side_effect=[Status('connecting'), Status('connected', 'Office')])
        status = cli.wait_status('connected', threading.Event(), time.monotonic() + 1)
        self.assertEqual(status.state, 'connected')

    def test_validation(self):
        for settings in (ProfileSettings('-option', 'host'), ProfileSettings('name\nInjected', 'host'),
                         ProfileSettings('name', ''), ProfileSettings('name', 'host', '2')):
            with self.assertRaises(VPNError):
                settings.validate()


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'state.json'
        self.env = patch.dict(os.environ, {'FAKE_STATE': str(self.path)})
        self.env.start()
        self.write_state()
        self.cli = CLI(FAKE, interactive_timeout=3)
        self.logs = []
        self.prompts = []
        self.cancel = threading.Event()

    def tearDown(self):
        self.env.stop()
        self.temp.cleanup()

    def write_state(self, mode='', **extra):
        self.path.write_text(json.dumps({'profiles': {'Office VPN': dict(FIELDS)}, 'mode': mode, **extra}))

    def answer(self, kind):
        self.prompts.append(kind)
        return {'password': 'SUPERSECRET', 'mfa': '123SECRET', 'username': 'worker'}[kind]

    def connect(self):
        return self.cli.connect(self.cli.view(Profile('Office VPN')), 'worker', True,
                                self.cancel, self.answer, self.logs.append)

    def test_connect_and_credentials_not_logged_or_argv(self):
        self.assertEqual(self.connect().state, 'connected')
        self.assertEqual(self.prompts, ['password', 'mfa'])
        diagnostic = '\n'.join(self.logs) + self.path.with_suffix('.args').read_text()
        self.assertNotIn('SUPERSECRET', diagnostic)
        self.assertNotIn('123SECRET', diagnostic)
        calls = [json.loads(line) for line in self.path.with_suffix('.args').read_text().splitlines()]
        command = next(c for c in calls if c[1] == 'connect')
        self.assertIn('-s', command)
        self.assertEqual(command[2], 'Office VPN')

    def test_mfa_retry(self):
        self.write_state('retry')
        self.assertEqual(self.connect().state, 'connected')
        self.assertEqual(self.prompts.count('mfa'), 2)

    def test_rejected_password(self):
        self.write_state('rejected')
        with self.assertRaises(VPNError):
            self.connect()
        self.assertEqual(self.cli.status().state, 'disconnected')

    def test_no_false_connection_success(self):
        self.write_state('false_success')
        with self.assertRaisesRegex(VPNError, 'did not confirm'):
            self.connect()

    def test_unknown_certificate_prompt(self):
        self.write_state('unknown')
        with self.assertRaisesRegex(VPNError, 'unsupported response'):
            self.connect()
        self.assertEqual(self.prompts, [])

    def test_cancel(self):
        self.write_state('hang')
        timer = threading.Timer(.2, self.cancel.set)
        timer.start()
        with self.assertRaises(Cancelled):
            self.connect()
        timer.join()

    def test_timeout(self):
        self.write_state('hang')
        self.cli.interactive_timeout = .2
        with self.assertRaisesRegex(VPNError, 'timed out'):
            self.connect()

    def test_cancel_password_dialog(self):
        with self.assertRaises(Cancelled):
            self.cli.connect(self.cli.view(Profile('Office VPN')), '', False,
                             self.cancel, lambda _: None, self.logs.append)

    def test_crud_and_preserve_advanced_settings(self):
        existing = self.cli.view(Profile('Office VPN'))
        saved = self.cli.save_profile(ProfileSettings('Office VPN', 'new.example:443'), existing,
                    self.cancel, self.answer, self.logs.append)
        self.assertEqual(saved.gateway, 'new.example:443')
        self.assertEqual(saved.username, 'worker')
        self.assertEqual(saved.fields['Auto Connect'], 'Enabled')
        self.cli.save_profile(ProfileSettings('Second VPN', 'localhost:443', '2', 'newuser', '3'), None,
                              self.cancel, self.answer, self.logs.append)
        self.assertEqual(self.cli.view(Profile('Second VPN')).username, 'newuser')
        self.cli.remove(Profile('Second VPN'))
        self.assertEqual([p.name for p in self.cli.list_profiles()], ['Office VPN'])

    def test_false_save_and_delete(self):
        self.write_state('false_save')
        with self.assertRaises(VPNError):
            self.cli.save_profile(ProfileSettings('Office VPN', 'changed'), self.cli.view(Profile('Office VPN')),
                                 self.cancel, self.answer, self.logs.append)
        self.write_state('false_remove')
        with self.assertRaises(VPNError):
            self.cli.remove(Profile('Office VPN'))

    def test_duplicate_creation(self):
        with self.assertRaisesRegex(VPNError, 'already exists'):
            self.cli.save_profile(ProfileSettings('Office VPN', 'host'), None, self.cancel, self.answer, self.logs.append)

    def test_connected_and_managed_profiles_protected(self):
        self.write_state(status='Connected', active='Office VPN')
        with self.assertRaises(VPNError):
            self.cli.remove(Profile('Office VPN'))
        with self.assertRaises(VPNError):
            self.cli.remove(Profile('Company', managed=True))
        with self.assertRaises(VPNError):
            self.connect()

    def test_disconnect_verification(self):
        self.write_state(status='Connected', active='Office VPN')
        self.assertEqual(self.cli.disconnect().state, 'disconnected')
        self.write_state('false_disconnect', status='Connected', active='Office VPN')
        with self.assertRaises(VPNError):
            self.cli.disconnect()

    def test_cli_discovery_falls_back_to_official_location_and_recovers(self):
        cli = CLI()
        with patch('forti_vpn.backend.shutil.which', return_value=None), patch('forti_vpn.backend.os.path.isfile', return_value=True), patch('forti_vpn.backend.os.access', return_value=True):
            self.assertEqual(cli.resolve_executable(), '/opt/forticlient/forticlient-cli')
        with patch('forti_vpn.backend.shutil.which', return_value=None), patch('forti_vpn.backend.os.path.isfile', return_value=False):
            with self.assertRaises(CLINotFound):
                cli.resolve_executable()
        with patch('forti_vpn.backend.shutil.which', return_value='/usr/bin/forticlient'):
            self.assertEqual(cli.resolve_executable(), '/usr/bin/forticlient')

    def test_unavailable(self):
        with self.assertRaisesRegex(CLINotFound, 'CLI was not found'):
            CLI('/does/not/exist').status()
        with self.assertRaisesRegex(CLINotFound, 'CLI was not found'):
            CLI('/does/not/exist').interactive(['connect', 'test'], [], lambda _: '', self.cancel, self.logs.append)

    def test_read_timeout(self):
        self.cli.read_timeout = .001
        with self.assertRaisesRegex(VPNError, 'did not respond'):
            self.cli.status()

    def test_malformed_output(self):
        self.write_state('malformed')
        with self.assertRaises(VPNError):
            self.cli.list_profiles()


class PreferenceTests(unittest.TestCase):
    def test_migration_and_settings_preserved_on_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'prefs.json'
            path.write_text(json.dumps({'last_profile': 'Old', 'password': 'secret', 'appearance': 'invalid'}))
            prefs = Preferences(path)
            self.assertEqual(prefs.data['appearance'], 'system')
            prefs.set('close_behavior', 'quit')
            prefs.set('appearance', 'dark')
            prefs.set('remember_password', True)
            prefs.select('New')
            loaded = Preferences(path)
            self.assertEqual(loaded.data, {'last_profile': 'New', 'close_behavior': 'quit', 'appearance': 'dark', 'remember_password': True})
            self.assertNotIn('secret', path.read_text())

    def test_only_profile_persisted(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'preferences.json'
            preferences = Preferences(path)
            preferences.select('Office VPN')
            self.assertEqual(json.loads(path.read_text())['last_profile'], 'Office VPN')
            self.assertNotIn('password', json.loads(path.read_text()))
            self.assertEqual(Preferences(path).last_profile, 'Office VPN')
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            path.write_text('bad json')
            self.assertEqual(Preferences(path).last_profile, '')


if __name__ == '__main__':
    unittest.main()
