"""Validated, non-secret application preferences, with backwards compatibility."""
import json
import os
from pathlib import Path

DEFAULTS = {'last_profile': '', 'close_behavior': 'hide', 'appearance': 'system',
            'remember_password': False}


class Preferences:
    def __init__(self, path=None):
        self.path = Path(path) if path else Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config')) / 'forti-vpn' / 'preferences.json'
        self.data = dict(DEFAULTS)
        try:
            loaded = json.loads(self.path.read_text())
            if isinstance(loaded, dict):
                for key, value in loaded.items():
                    if self.valid(key, value):
                        self.data[key] = value
        except (OSError, ValueError):
            pass

    @staticmethod
    def valid(key, value):
        return ((key == 'last_profile' and isinstance(value, str)) or
                (key == 'close_behavior' and value in ('hide', 'quit')) or
                (key == 'appearance' and value in ('system', 'light', 'dark')) or
                (key == 'remember_password' and isinstance(value, bool)))

    @property
    def last_profile(self):
        return self.data['last_profile']

    def set(self, key, value):
        if not self.valid(key, value):
            raise ValueError('Invalid preference')
        updated = {**self.data, key: value}
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        temporary = self.path.with_suffix('.tmp')
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, 'w') as stream:
            json.dump(updated, stream)
        temporary.replace(self.path)
        self.data = updated

    def select(self, name):
        self.set('last_profile', name)
