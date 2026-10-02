"""Check the downloadable package independently of a live VPN."""
import hashlib
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


@unittest.skipUnless(shutil.which('dpkg-deb'), 'Requires dpkg-deb')
class PackageTests(unittest.TestCase):
    def test_package_is_reproducible_and_contains_only_app_files(self):
        project = Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            command = ['/usr/bin/python3', str(project / 'scripts/build-deb.py'), '--output', str(output)]
            subprocess.run(command, check=True, capture_output=True)
            package = next(output.glob('*.deb'))
            first = package.read_bytes()
            subprocess.run(command, check=True, capture_output=True)
            self.assertEqual(first, package.read_bytes())
            self.assertEqual((output / 'SHA256SUMS').read_text(),
                             hashlib.sha256(first).hexdigest() + '  ' + package.name + '\n')
            root = Path(directory) / 'root'
            subprocess.run(['dpkg-deb', '--extract', str(package), str(root)], check=True)
            self.assertTrue((root / 'usr/bin/forti-vpn').stat().st_mode & 0o111)
            desktop = (root / 'usr/share/applications/io.github.fortivpn.Utility.desktop').read_text()
            self.assertIn('Exec=/usr/bin/forti-vpn', desktop)
            self.assertEqual((root / 'usr/share/doc/forti-vpn/LICENSE').read_bytes(), (project / 'LICENSE').read_bytes())
            self.assertTrue((root / 'usr/share/doc/forti-vpn/NOTICE').exists())
            self.assertTrue((root / 'usr/lib/forti-vpn/forti_vpn/assets/io.github.fortivpn.Utility-connected.svg').exists())
            for path in root.rglob('*'):
                self.assertNotIn('__pycache__', path.parts)
                self.assertNotIn('.cache', path.parts)
                self.assertFalse(path.suffix in ('.pyc', '.pyo'))
                self.assertNotIn('opt', path.relative_to(root).parts)
            architecture = subprocess.check_output(['dpkg-deb', '--field', str(package), 'Architecture'], text=True).strip()
            self.assertEqual(architecture, 'all')
