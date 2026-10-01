#!/usr/bin/env python3
"""Fake subprocess peer. No credentials are written to disk."""
import json
import os
from pathlib import Path
import sys
import time

path = Path(os.environ['FAKE_STATE'])
state = json.loads(path.read_text())
args = sys.argv[1:]
with path.with_suffix('.args').open('a') as stream:
    stream.write(json.dumps(args) + '\n')
command = args[1]
mode = state.get('mode', '')


def write():
    path.write_text(json.dumps(state))


def prompt(text):
    # Split prompts deliberately to exercise the PTY reader.
    midpoint = len(text) // 2
    print(text[:midpoint], end='', flush=True)
    time.sleep(.03)
    print(text[midpoint:], end='', flush=True)
    return sys.stdin.readline().rstrip('\n')


if command == 'list':
    if mode == 'malformed':
        print('nonsense')
    else:
        print('VPNs:\n  Personal VPNs:')
        for name in state['profiles']:
            print('    ' + name)
elif command == 'status':
    print('Status: ' + state.get('status', 'Not Running'))
    if state.get('active'):
        print('VPN Name: ' + state['active'])
elif command == 'view':
    name = args[2]
    profile = state['profiles'].get(name)
    if not profile:
        sys.exit(1)
    print('VPN: ' + name)
    for key, value in profile.items():
        print('  ' + key + ': ' + value)
elif command == 'connect':
    if mode == 'hang':
        time.sleep(10)
    if mode == 'unknown':
        prompt('Accept untrusted certificate? [y/n] ')
        sys.exit(1)
    secret = prompt('Please input password.\n')
    if mode == 'login_blocked':
        Path(os.environ['FAKE_VPN_LOG']).write_text('Login failed: Too many bad login attempts.\npassword SUPERSECRET token 123SECRET\n')
        print('Authentication failed')
        sys.exit(0)
    if mode == 'profile_load_failure':
        print('Status: Disconnected')
        print('Notification: Load VPN profile was failed. Check log file for details.')
        print('DONE.')
        sys.exit(0)
    if mode == 'rejected':
        print('Incorrect password')
        sys.exit(1)
    code = prompt('Enter verification code: ')
    if mode == 'retry':
        print('Invalid code; try again')
        code = prompt('Enter verification code: ')
    if mode not in ('false_success', 'keyring_false_success'):
        state['status'] = 'Connected'
        state['active'] = args[2]
        write()
    if mode in ('keyring_warning', 'keyring_false_success'):
        print('Failed to save credential to key store.')
    print('DONE.')
elif command == 'disconnect':
    if mode != 'false_disconnect':
        state['status'] = 'Not Running'
        state.pop('active', None)
        write()
    print('DONE.')
elif command == 'remove':
    if mode != 'false_remove':
        state['profiles'].pop(args[2], None)
        write()
    print('DONE.')
elif command == 'edit':
    name = args[2]
    old = state['profiles'].get(name, {})
    gateway = prompt('Remote Gateway [current=' + old.get('Remote Gateway', '') + ']: ')
    authentication = prompt('Authentication (1.prompt / 2.save / 3.disable) [current=save]: ') or '2'
    username = old.get('Username', '')
    if authentication == '2':
        username = prompt('  Username: [current=' + username + ']') or username
    cert_type = prompt('Certificate Type (1.local (pkcs12) / 2.smartcard (pkcs11) / 3.disable) [current=disable]: ')
    certificate = old.get('Client Certificate', 'None')
    if cert_type in ('1', '2'):
        certificate = prompt('Client Certificate path (.p12 file URL) [default=None]: ')
    if mode != 'false_save':
        state['profiles'][name] = {**old, 'Remote Gateway': gateway,
                                   'Username': username, 'Client Certificate': certificate}
        write()
    print('DONE.')
