#!/usr/bin/python3
"""Build a standalone Ubuntu package without root or bundled dependencies."""
import argparse
import ast
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def build(output):
    project = Path(__file__).resolve().parents[1]
    tree = ast.parse((project / 'forti_vpn/__init__.py').read_text())
    version = next(ast.literal_eval(node.value) for node in tree.body
                   if isinstance(node, ast.Assign) and any(
                       isinstance(target, ast.Name) and target.id == '__version__' for target in node.targets))
    subprocess.run(['dpkg', '--validate-version', version], check=True)
    epoch = int(os.environ.get('SOURCE_DATE_EPOCH') or subprocess.check_output(
        ['git', 'log', '-1', '--format=%ct'], cwd=project, text=True).strip())
    output.mkdir(parents=True, exist_ok=True)
    package = output / f'forti-vpn_{version}_all.deb'
    with tempfile.TemporaryDirectory(prefix='forti-vpn-deb-') as temporary:
        root = Path(temporary)
        def write(path, contents, executable=False):
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(contents)
            target.chmod(0o755 if executable else 0o644)

        for source in sorted((project / 'forti_vpn').rglob('*')):
            if not source.is_file() or '__pycache__' in source.parts or source.suffix in ('.pyc', '.pyo'):
                continue
            target = root / 'usr/lib/forti-vpn/forti_vpn' / source.relative_to(project / 'forti_vpn')
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o644)
        write('usr/bin/forti-vpn', '#!/usr/bin/python3\nimport runpy\nimport sys\n'
              'sys.path.insert(0, "/usr/lib/forti-vpn")\n'
              'runpy.run_module("forti_vpn", run_name="__main__")\n', executable=True)
        write('usr/share/applications/io.github.fortivpn.Utility.desktop',
              (project / 'data/io.github.fortivpn.Utility.desktop').read_text().replace(
                  'Exec=forti-vpn', 'Exec=/usr/bin/forti-vpn'))
        for variant, directory in (('', 'scalable'), ('-symbolic', 'symbolic')):
            name = 'io.github.fortivpn.Utility' + variant + '.svg'
            write(f'usr/share/icons/hicolor/{directory}/apps/{name}', (project / 'data' / name).read_text())
        for name in ('LICENSE', 'NOTICE', 'README.md'):
            write('usr/share/doc/forti-vpn/' + name, (project / name).read_text())
        for source in sorted((project / 'docs/screenshots').glob('*.png')):
            target = root / 'usr/share/doc/forti-vpn/docs/screenshots' / source.name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o644)
        write('usr/share/doc/forti-vpn/copyright',
              'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n'
              'Upstream-Name: Forti-VPN\nSource: https://github.com/othman2408/forti-vpn\n\n'
              'Files: *\nCopyright: 2026 othman2408\nLicense: MIT\n' +
              ''.join(' ' + (line or '.') + '\n' for line in (project / 'LICENSE').read_text().splitlines()))
        size = (sum(path.stat().st_size for path in root.rglob('*') if path.is_file()) + 1023) // 1024
        write('DEBIAN/control', f'Package: forti-vpn\nVersion: {version}\nArchitecture: all\n'
              'Section: net\nPriority: optional\nMaintainer: othman2408 <acerxr@gmail.com>\n'
              f'Installed-Size: {size}\n'
              'Depends: python3 (>= 3.10), python3-gi, gir1.2-gtk-4.0 (>= 4.14), gir1.2-adw-1 (>= 1.5)\n'
              'Recommends: gnome-shell-extension-appindicator\n'
              'Homepage: https://github.com/othman2408/forti-vpn\n'
              'Description: Native Ubuntu GUI for FortiClient VPN\n'
              ' Connect and disconnect, manage profiles, and view sanitized diagnostics.\n'
              ' Designed for Ubuntu 26.04 and FortiClient 7.4.3. FortiClient must be\n'
              ' installed separately; guided setup is available when its CLI is missing.\n')
        files = [path for path in sorted(root.rglob('*')) if path.is_file() and 'DEBIAN' not in path.relative_to(root).parts]
        write('DEBIAN/md5sums', ''.join(
            hashlib.md5(path.read_bytes(), usedforsecurity=False).hexdigest() + '  ' + str(path.relative_to(root)) + '\n'
            for path in files))
        for path in sorted(root.rglob('*')):
            if path.is_dir():
                path.chmod(0o755)
            os.utime(path, (epoch, epoch))
        root.chmod(0o755)
        os.utime(root, (epoch, epoch))
        subprocess.run(['dpkg-deb', '--root-owner-group', '-Zxz', '--build', str(root), str(package)],
                       check=True, env={**os.environ, 'SOURCE_DATE_EPOCH': str(epoch)})
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    (output / 'SHA256SUMS').write_text(digest + '  ' + package.name + '\n')
    print(package)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=Path(__file__).resolve().parents[1] / 'dist')
    build(parser.parse_args().output.resolve())
