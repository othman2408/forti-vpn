"""Asynchronous workflow state; GTK is injected through dispatch callbacks."""
from dataclasses import dataclass, field
import queue
import threading
import time
from .backend import CLI, CLINotFound, Status, VPNError


@dataclass
class State:
    status: Status = field(default_factory=lambda: Status('unavailable'))
    profiles: list = field(default_factory=list)
    busy: str | None = None
    error: str = ''
    error_origin: str = ''
    setup_required: bool = False
    diagnostics: list[str] = field(default_factory=list)


@dataclass
class Challenge:
    kind: str
    responses: queue.Queue = field(default_factory=lambda: queue.Queue(maxsize=1))

    def respond(self, value):
        try:
            self.responses.put_nowait(value)
        except queue.Full:
            pass


class Workflow:
    def __init__(self, dispatch, changed, prompt, cli=None, on_complete=None):
        self.cli = cli or CLI()
        self.dispatch = dispatch
        self.changed = changed
        self.prompt = prompt
        self.on_complete = on_complete
        self.state = State()
        self.cancel_event = threading.Event()
        self.closed = False
        self.refreshing = False
        self.deadline = 0
        self.operation = None
        self.revision = 0

    def notify(self):
        if not self.closed:
            self.changed(self.state)

    def log(self, text):
        def add():
            self.state.diagnostics.append(time.strftime('%H:%M:%S') + ' ' + text)
            self.state.diagnostics = self.state.diagnostics[-100:]
            self.notify()
        self.dispatch(add)

    def ask(self, kind):
        challenge = Challenge(kind)
        def present():
            if not self.closed and not self.cancel_event.is_set():
                self.state.busy = 'awaiting authentication'
                self.notify()
                self.prompt(challenge)
        self.dispatch(present)
        while not self.cancel_event.is_set() and time.monotonic() < self.deadline:
            try:
                value = challenge.responses.get(timeout=.1)
                self.dispatch(lambda: self._resume())
                return value
            except queue.Empty:
                pass
        if time.monotonic() >= self.deadline:
            self.cancel_event.set()
            raise VPNError('Authentication timed out. Retry when you are ready to sign in.')
        return None

    def _resume(self):
        self.state.busy = self.operation
        self.notify()

    def cancel(self):
        self.cancel_event.set()

    def refresh(self):
        if self.state.busy or self.closed or self.refreshing:
            return
        self._start('refreshing', self._snapshot, refresh=True)

    def _snapshot(self):
        return self.cli.status(), [self.cli.view(p) for p in self.cli.list_profiles()]

    def _start(self, operation, work, refresh=False):
        if self.state.busy or self.closed:
            return
        if refresh:
            self.refreshing = True
        else:
            self.state.busy = operation
            self.operation = operation
            self.revision += 1
        revision = self.revision
        if not refresh:
            self.state.error = ''
        self.cancel_event = threading.Event()
        self.deadline = time.monotonic() + self.cli.interactive_timeout
        self.notify()
        def run():
            error = ''
            setup_required = False
            snapshot = None
            try:
                result = work()
                snapshot = result if refresh else self._snapshot()
            except (VPNError, OSError, ValueError) as exc:
                error = str(exc)
                setup_required = isinstance(exc, CLINotFound)
                try:
                    snapshot = self._snapshot()
                    setup_required = False
                except (VPNError, OSError, ValueError) as snapshot_error:
                    setup_required = isinstance(snapshot_error, CLINotFound)
                    snapshot = (Status('unavailable'), self.state.profiles)
            except Exception:
                error = 'An unexpected application error occurred. Restart the app and retry.'
                snapshot = (Status('unavailable'), self.state.profiles)
            def finish():
                if self.closed:
                    return
                if refresh:
                    self.refreshing = False
                    if self.state.busy or revision != self.revision:
                        return
                else:
                    self.state.busy = None
                self.state.status, self.state.profiles = snapshot
                self.state.setup_required = setup_required
                if error:
                    self.state.error = error
                    self.state.error_origin = operation
                    self.state.diagnostics.append(time.strftime('%H:%M:%S') + ' ' + error)
                elif not refresh or self.state.error_origin == 'refreshing':
                    self.state.error = ''
                    self.state.error_origin = ''
                self.state.diagnostics = self.state.diagnostics[-100:]
                self.notify()
                if not refresh and self.on_complete:
                    self.on_complete(operation, not bool(error))
            self.dispatch(finish)
        threading.Thread(target=run, daemon=True).start()

    def connect(self, profile, username, remember):
        self._start('connecting', lambda: self.cli.connect(profile, username, remember,
                    self.cancel_event, self.ask, self.log))

    def disconnect(self):
        self._start('disconnecting', self.cli.disconnect)

    def save(self, settings, existing):
        self._start('saving profile', lambda: self.cli.save_profile(settings, existing,
                    self.cancel_event, self.ask, self.log))

    def remove(self, profile):
        self._start('removing profile', lambda: self.cli.remove(profile))

    def close(self):
        self.closed = True
        self.cancel_event.set()
