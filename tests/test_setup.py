from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
import runpy
import sys
from types import ModuleType, SimpleNamespace
from unittest.mock import patch
from forti_vpn.installation import install_command, package_description


class SetupTests(unittest.TestCase):
    def test_install_command_quotes_selected_package_without_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "FortiClient ' $(touch unwanted).deb"
            path.touch()
            command = install_command(str(path))
            self.assertEqual(shlex.split(command), ['sudo', 'apt', 'install', str(path)])
            self.assertFalse((Path(directory) / 'unwanted').exists())
            with self.assertRaises(ValueError):
                install_command('relative.deb')
            with self.assertRaises(ValueError):
                install_command(str(Path(directory) / 'missing.deb'))
            other = Path(directory) / 'installer.sh'
            other.touch()
            with self.assertRaises(ValueError):
                install_command(str(other))

    def test_missing_gui_dependencies_give_instructions_instead_of_traceback(self):
        result = subprocess.run(['/usr/bin/python3', '-S', '-m', 'forti_vpn'], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('sudo apt install', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_old_gui_libraries_give_upgrade_instructions(self):
        gi = ModuleType('gi')
        gi.require_version = lambda *args: None
        repository = ModuleType('gi.repository')
        repository.Gtk = SimpleNamespace()
        repository.Adw = SimpleNamespace()
        with patch.dict(sys.modules, {'gi': gi, 'gi.repository': repository}):
            with self.assertRaisesRegex(SystemExit, 'packages are too old'):
                runpy.run_module('forti_vpn', run_name='__main__')

    def test_download_guidance_matches_architecture(self):
        with patch('forti_vpn.installation.platform.machine', return_value='x86_64'):
            self.assertIn('amd64', package_description())
        with patch('forti_vpn.installation.platform.machine', return_value='aarch64'):
            self.assertIn('ARM64', package_description())
