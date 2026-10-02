import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import gi
gi.require_version('Gtk', '4.0')
gi.require_version('Adw', '1')
from forti_vpn.backend import CLI, Status
from forti_vpn.support import client_information, troubleshooting_report


class SupportTests(unittest.TestCase):
    def test_version_output_drops_device_identifiers(self):
        cli = CLI()
        output = 'FortiClient Version: 7.4.3.5411\nFortiClient Serial: SECRET-SERIAL\nFortiClient UID: SECRET-UID\n'
        with patch.object(cli, 'resolve_executable', return_value='/opt/forticlient/forticlient-cli'), \
                patch('forti_vpn.backend.subprocess.run', return_value=subprocess.CompletedProcess([], 0, output)) as run:
            info = client_information(cli)
        self.assertEqual(info['version'], '7.4.3.5411')
        self.assertNotIn('SECRET', str(info))
        self.assertEqual(run.call_args.args[0], ['/opt/forticlient/forticlient-cli', 'version'])
        parent = SimpleNamespace(indicator_available=True, workflow=SimpleNamespace(state=SimpleNamespace(
            status=Status('connected', 'PRIVATE-PROFILE'), diagnostics=['12:00:00 FortiClient requested password.'])))
        report = troubleshooting_report(parent, info)
        self.assertIn('VPN status: connected', report)
        self.assertNotIn('PRIVATE-PROFILE', report)
        self.assertNotIn('SECRET', report)

    def test_failure_never_includes_raw_output(self):
        cli = CLI()
        with patch.object(cli, 'resolve_executable', return_value='/usr/bin/forticlient'), \
                patch('forti_vpn.backend.subprocess.run', return_value=subprocess.CompletedProcess([], 1, 'SECRET')):
            self.assertEqual(client_information(cli)['version'], 'Could not determine')
        with patch.object(cli, 'resolve_executable', return_value='/usr/bin/forticlient'), \
                patch('forti_vpn.backend.subprocess.run', side_effect=subprocess.TimeoutExpired('forticlient', 10)):
            self.assertEqual(client_information(cli)['version'], 'Unavailable')
