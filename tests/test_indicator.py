"""Real session-bus protocol checks with callbacks that never touch a VPN."""
import os
from pathlib import Path
import subprocess
import time
import unittest


@unittest.skipUnless(os.environ.get('RUN_GUI_TESTS') == '1', 'Requires a graphical session bus')
class IndicatorTests(unittest.TestCase):
    def setUp(self):
        from forti_vpn.indicator import Indicator
        from gi.repository import GLib
        self.GLib = GLib
        self.events = []
        self.item = Indicator(lambda: self.events.append('show'), lambda: self.events.append('settings'),
                              lambda: self.events.append('disconnect'), lambda: self.events.append('quit'),
                              lambda available: None, str(Path('forti_vpn/assets').resolve()))
        self.item.update('Connected', True)

    def tearDown(self):
        self.item.close()

    def call(self, path, method, *args):
        process = subprocess.Popen(['gdbus', 'call', '--session', '--dest', self.item.connection.get_unique_name(),
                                    '--object-path', path, '--method', method, '--', *args],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.monotonic() + 3
        while process.poll() is None and time.monotonic() < deadline:
            while self.GLib.MainContext.default().pending():
                self.GLib.MainContext.default().iteration(False)
            time.sleep(.005)
        if process.poll() is None:
            process.kill()
        output, error = process.communicate()
        self.assertEqual(process.returncode, 0, error)
        return output

    def test_layout_and_status_properties(self):
        output = self.call('/Menu', 'com.canonical.dbusmenu.GetLayout', '0', '-1', '[]')
        self.assertIn('Disconnect and Quit', output)
        self.assertIn('Connected', output)
        output = self.call('/StatusNotifierItem', 'org.freedesktop.DBus.Properties.GetAll', 'org.kde.StatusNotifierItem')
        self.assertIn('io.github.fortivpn.Utility-symbolic', output)
        self.assertIn('Connected', output)

    def test_menu_callbacks_and_activation(self):
        for ident, expected in [(2, 'show'), (3, 'settings'), (4, 'disconnect'), (6, 'quit')]:
            self.call('/Menu', 'com.canonical.dbusmenu.Event', str(ident), 'clicked', "<'unused'>", '0')
            self.assertEqual(self.events[-1], expected)
        self.item.update('Disconnected', False)
        self.events.clear()
        self.call('/Menu', 'com.canonical.dbusmenu.Event', '4', 'clicked', "<'unused'>", '0')
        self.assertEqual(self.events, [])
        self.call('/StatusNotifierItem', 'org.kde.StatusNotifierItem.Activate', '0', '0')
        self.assertEqual(self.events, ['show'])

    def test_filtered_properties(self):
        output = self.call('/Menu', 'com.canonical.dbusmenu.GetGroupProperties', '[2, 3]', "['label']")
        self.assertIn('Open Forti VPN', output)
        self.assertNotIn('enabled', output)
