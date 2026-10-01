"""FortiClient CLI boundary. No GTK imports and no shell execution."""
from __future__ import annotations

from dataclasses import dataclass, field
import errno
import os
import pty
import re
import select
import signal
import subprocess
import termios
import threading
import time


class VPNError(Exception):
    pass


class Cancelled(VPNError):
    pass


@dataclass(frozen=True)
class Profile:
    name: str
    fields: dict[str, str] = field(default_factory=dict)
    managed: bool = False

    @property
    def username(self):
        return self.fields.get('Username', '')

    @property
    def gateway(self):
        return self.fields.get('Remote Gateway', '')


@dataclass(frozen=True)
class Status:
    state: str
    profile: str | None = None


@dataclass(frozen=True)
class ProfileSettings:
    name: str
    gateway: str
    authentication: str = ''  # empty means preserve the CLI's current value
    username: str = ''
    certificate_type: str = ''
    certificate: str = ''

    def validate(self):
        for value in vars(self).values():
            if any(c in value for c in ('\n', '\r', '\x00')):
                raise VPNError('Profile fields must contain a single line.')
        if not self.name.strip() or self.name.startswith('-'):
            raise VPNError('Enter a profile name that does not start with a dash.')
        if not self.gateway.strip():
            raise VPNError('Enter a remote gateway.')
        if self.authentication not in ('', '1', '2', '3'):
            raise VPNError('Invalid authentication mode.')
        if self.certificate_type not in ('', '1', '2', '3'):
            raise VPNError('Invalid certificate type.')
        if self.authentication == '2' and not self.username.strip():
            raise VPNError('Enter a username for saved authentication.')
        if self.certificate_type in ('1', '2') and not self.certificate.strip():
            raise VPNError('Enter a certificate path or PKCS#11 library.')


def parse_profiles(output: str) -> list[Profile]:
    if not re.search(r'^VPNs:', output, re.M):
        raise VPNError('FortiClient returned an unexpected profile list.')
    result = []
    managed = False
    section = False
    for line in output.splitlines()[1:]:
        if not line.strip():
            continue
        if re.match(r'^  \S.*:\s*$', line):
            section = True
            managed = 'personal' not in line.lower()
        elif re.match(r'^    \S', line) and section:
            result.append(Profile(line.strip(), managed=managed))
        else:
            raise VPNError('FortiClient returned an unexpected profile list.')
    return result


def parse_profile(name: str, output: str, managed=False) -> Profile:
    if not output.startswith('VPN: ' + name + '\n'):
        raise VPNError('FortiClient could not read this profile.')
    fields = {}
    for line in output.splitlines()[1:]:
        key, sep, value = line.strip().partition(':')
        if sep and value.strip() and not re.search('password|secret|token', key, re.I):
            fields[key] = value.strip()
    return Profile(name, fields, managed)


def parse_status(output: str) -> Status:
    match = re.search(r'^\s*Status:\s*(.*?)\s*$', output, re.M)
    if not match:
        raise VPNError('FortiClient returned an unexpected status. Open diagnostics for details.')
    value = match[1].lower().removesuffix('...')
    states = {'not running': 'disconnected', 'disconnected': 'disconnected',
              'connected': 'connected', 'connecting': 'connecting',
              'disconnecting': 'disconnecting', 're-connecting': 'connecting',
              'xauth': 'awaiting authentication'}
    if value not in states:
        raise VPNError('Unsupported FortiClient status. Refresh status or check the original CLI.')
    name = re.search(r'^\s*(?:VPN(?: name)?|VPN Name|Name|Profile|VPN tunnel):\s*(.+?)\s*$', output, re.M | re.I)
    return Status(states[value], name[1] if name else None)


# The editor expressions are based on the installed 7.4.3 dialogue.
EDIT_PROMPTS = (
    ('gateway', r'Remote Gateway(?: \[(?:current|default)=[^\r\n]*\])?:\s*$'),
    ('authentication', r'Authentication \(1\.prompt / 2\.save / 3\.disable\) \[(?:current|default)=[^\r\n]*\]:\s*$'),
    ('username', r'Username:\s*(?:\[(?:current|default)=[^\r\n]*\])?\s*$'),
    ('certificate_type', r'Certificate Type \(1\.local \(pkcs12\) / 2\.smartcard \(pkcs11\) / 3\.disable\) \[(?:current|default)=[^\r\n]*\]:\s*$'),
    ('certificate_password', r'(?:^|\n)[ \t]*Client Certificate password:\s*$'),
    ('verify_password', r'(?:^|\n)[ \t]*Verify Password:\s*$'),
    ('certificate', r'(?:Client Certificate path \(\.p12 file URL\)|PKCS#11 library \(library name or file URL\)) \[(?:current|default)=[^\r\n]*\]:\s*$'),
)
AUTH_PROMPTS = (
    ('certificate_password', r'(?:^|\n)[ \t]*Client Certificate password:\s*$'),
    ('verify_password', r'(?:^|\n)[ \t]*Verify Password:\s*$'),
    ('password', r'(?:^|\n)[ \t]*(?:Please input password\.|(?:Enter |Input |Please enter )?(?:VPN )?password\s*:)\s*$'),
    ('username', r'(?:^|\n)[ \t]*(?:Please input username\.|(?:Enter |Input )?(?:user ?name|username)\s*:)\s*$'),
    ('mfa', r'(?:^|\n)[ \t]*(?:Please input (?:token|verification code|OTP)\.|(?:Enter |Input |Please enter )?(?:FortiToken|token|verification code|one[- ]time (?:password|code)|OTP|two[- ]factor (?:code|token))\s*:)\s*$'),
)


class CLI:
    def __init__(self, executable='forticlient', read_timeout=10, interactive_timeout=120):
        self.executable = executable
        self.read_timeout = read_timeout
        self.interactive_timeout = interactive_timeout

    def read(self, *args):
        try:
            result = subprocess.run([self.executable, 'vpn', *args], capture_output=True,
                                    text=True, timeout=self.read_timeout, env={**os.environ, 'LC_ALL': 'C'})
        except FileNotFoundError as exc:
            raise VPNError('FortiClient is not installed or is not on PATH.') from exc
        except subprocess.TimeoutExpired as exc:
            raise VPNError('FortiClient did not respond within ten seconds.') from exc
        if result.returncode:
            raise VPNError('FortiClient could not complete ' + args[0] + '. Check its service and permissions.')
        return result.stdout.replace('\r\n', '\n')

    def list_profiles(self):
        return parse_profiles(self.read('list'))

    def view(self, profile):
        return parse_profile(profile.name, self.read('view', profile.name), profile.managed)

    def status(self):
        return parse_status(self.read('status'))

    def wait_status(self, expected, cancel, deadline):
        while True:
            if cancel.is_set():
                raise Cancelled('Operation cancelled. Checking VPN status…')
            status = self.status()
            if status.state == expected:
                return status
            if status.state not in ('connecting', 'disconnecting', 'awaiting authentication') or time.monotonic() >= deadline:
                raise VPNError('FortiClient did not confirm ' + expected + '. Refresh status and try again.')
            cancel.wait(.25)

    def disconnect(self):
        self.read('disconnect')
        return self.wait_status('disconnected', threading.Event(), time.monotonic() + 10)

    def remove(self, profile):
        self.guard_profile(profile)
        self.read('remove', profile.name)
        if any(p.name == profile.name for p in self.list_profiles()):
            raise VPNError('FortiClient did not remove the profile.')

    def guard_profile(self, profile):
        if profile.managed:
            raise VPNError('Company-managed profiles cannot be changed in this app.')
        status = self.status()
        if status.state != 'disconnected':
            if status.profile is None or status.profile == profile.name:
                raise VPNError('Disconnect the active VPN before changing this profile.')

    def save_profile(self, settings, existing, cancel, ask, log):
        settings.validate()
        profile = existing or Profile(settings.name)
        self.guard_profile(profile)
        names = [p.name for p in self.list_profiles()]
        if existing is None and settings.name in names:
            raise VPNError('A profile with this name already exists.')
        if existing and existing.name != settings.name:
            raise VPNError('Profile renaming is not supported.')
        def respond(kind):
            if kind in ('certificate_password', 'verify_password'):
                return '' if existing and not settings.certificate_type else ask(kind)
            return getattr(settings, kind)
        self.interactive(['edit', settings.name], EDIT_PROMPTS, respond, cancel, log)
        saved = self.view(profile)
        if saved.gateway != settings.gateway:
            raise VPNError('FortiClient did not save the requested gateway. Review the profile before retrying.')
        if settings.authentication == '2' and saved.username != settings.username:
            raise VPNError('FortiClient did not save the requested username.')
        if settings.certificate_type in ('1', '2') and saved.fields.get('Client Certificate') != settings.certificate:
            raise VPNError('FortiClient did not save the requested certificate. Review the profile.')
        if existing:
            for key in ('Single Sign On (SSO) for VPN Tunnel', 'Auto Connect', 'Always Up'):
                if saved.fields.get(key) != existing.fields.get(key):
                    raise VPNError('FortiClient changed an advanced setting. Review the profile before connecting.')
        return saved

    def connect(self, profile, username, remember, cancel, ask, log):
        status = self.status()
        if status.state != 'disconnected':
            raise VPNError('A VPN operation is already active. Disconnect it before connecting again.')
        # --username is advertised by help but rejected by the installed delegate.
        args = ['connect', profile.name]
        if username:
            args += ['-u', username]
        if profile.fields.get('Save Password') != 'Enabled' or remember:
            args += ['-p']
        if remember:
            args += ['-s']
        deadline = time.monotonic() + self.interactive_timeout
        self.interactive(args, AUTH_PROMPTS, ask, cancel, log, deadline=deadline)
        return self.wait_status('connected', cancel, deadline)

    def interactive(self, args, prompts, answer, cancel, log, deadline=None):
        master, slave = pty.openpty()
        attr = termios.tcgetattr(slave)
        attr[3] &= ~(termios.ECHO | termios.ECHONL)
        termios.tcsetattr(slave, termios.TCSANOW, attr)
        process = None
        try:
            try:
                process = subprocess.Popen([self.executable, 'vpn', *args], stdin=slave,
                                           stdout=slave, stderr=slave, start_new_session=True,
                                           env={**os.environ, 'LC_ALL': 'C'})
            except FileNotFoundError as exc:
                raise VPNError('FortiClient is not installed or is not on PATH.') from exc
            os.close(slave)
            slave = -1
            compiled = [(kind, re.compile(pattern, re.I)) for kind, pattern in prompts]
            pending = ''
            deadline = deadline or time.monotonic() + self.interactive_timeout
            last_data = time.monotonic()
            log('Started FortiClient ' + args[0] + '.')
            while True:
                if cancel.is_set():
                    raise Cancelled('Operation cancelled. Checking VPN status…')
                if time.monotonic() > deadline:
                    cancel.set()
                    raise VPNError('The operation timed out. Retry when you are ready to authenticate.')
                if select.select([master], [], [], .1)[0]:
                    try:
                        chunk = os.read(master, 4096)
                    except OSError as exc:
                        if exc.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    pending += chunk.decode('utf-8', errors='replace').replace('\r', '')
                    pending = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', pending)[-16384:]
                    last_data = time.monotonic()
                matched = False
                for kind, pattern in compiled:
                    match = pattern.search(pending)
                    if match:
                        log('FortiClient requested ' + kind.replace('_', ' ') + '.')
                        value = answer(kind)
                        if value is None or cancel.is_set():
                            raise Cancelled('Operation cancelled. Checking VPN status…')
                        if any(c in value for c in ('\r', '\n', '\x00')):
                            raise VPNError('Responses must contain a single line.')
                        if time.monotonic() > deadline:
                            raise VPNError('The operation timed out.')
                        os.write(master, (value + '\n').encode())
                        value = None
                        pending = ''
                        matched = True
                        break
                if not matched and time.monotonic() - last_data > .6:
                    tail = pending.rstrip().split('\n')[-1]
                    if tail and (tail.endswith((':', '?', ']')) or re.search(r'(?:input|enter|accept|confirm|\[y/n\])', tail, re.I)):
                        raise VPNError('FortiClient requested an unsupported response. The operation was stopped safely.')
                if process.poll() is not None and not select.select([master], [], [], 0)[0]:
                    break
            code = process.wait(timeout=2)
            if code or re.search(r'\b(?:error|failed|failure|invalid|denied|unable|incorrect|unrecognized)\b', pending, re.I):
                raise VPNError('FortiClient rejected the operation. Check credentials, profile settings, and service availability.')
            log('FortiClient finished ' + args[0] + '; verifying the result.')
        finally:
            if process and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            os.close(master)
            if slave >= 0:
                os.close(slave)
