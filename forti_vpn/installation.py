"""Installation guidance shared by the setup UI and dependency checks."""
from pathlib import Path
import platform
import shlex

DOWNLOAD_URL = 'https://www.fortinet.com/support/product-downloads'
HELP_URL = 'https://docs.fortinet.com/document/forticlient/7.4.3/administration-guide/664703/installing-forticlient-on-computers'


def package_description():
    architecture = platform.machine().lower()
    if architecture in ('x86_64', 'amd64'):
        return 'Linux x86-64 (amd64) .deb'
    if architecture in ('aarch64', 'arm64'):
        return 'Linux ARM64 .deb'
    return 'Linux .deb for your computer'


def install_command(path):
    package = Path(path)
    if not package.is_absolute() or package.suffix.lower() != '.deb' or not package.is_file():
        raise ValueError('Choose the downloaded FortiClient .deb file from this computer.')
    return 'sudo apt install ' + shlex.quote(str(package))
