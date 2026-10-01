import queue
import threading
import time
import unittest
from unittest.mock import patch
from forti_vpn.backend import CLINotFound, Cancelled, Profile, Status, VPNError
from forti_vpn.workflow import Workflow


class FakeCLI:
    interactive_timeout = 1

    def __init__(self):
        self.current = Status('disconnected')
        self.unavailable = False
        self.read_gate = None

    def status(self):
        if self.unavailable:
            raise VPNError('FortiClient unavailable')
        return self.current

    def list_profiles(self):
        if self.read_gate:
            self.read_gate.wait(1)
        return [Profile('Office VPN')]

    def view(self, profile):
        return profile

    def connect(self, profile, username, remember, cancel, ask, log):
        answer = ask('password')
        if answer is None:
            raise Cancelled('Cancelled')
        ask('mfa')
        self.current = Status('connected', profile.name)
        return self.current

    def disconnect(self):
        self.current = Status('disconnected')
        return self.current


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.callbacks = queue.Queue()
        self.cli = FakeCLI()
        self.challenges = []
        self.workflow = Workflow(self.callbacks.put, lambda _: None, self.challenges.append, self.cli)

    def pump(self, until, timeout=2):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            try:
                self.callbacks.get(timeout=.02)()
            except queue.Empty:
                pass
            if until():
                return
        self.fail('Workflow did not reach expected state')

    def refresh(self):
        self.workflow.refresh()
        self.pump(lambda: not self.workflow.refreshing)

    def test_warning_events_surface_without_connection_failure(self):
        self.workflow.log('Warning: Password could not be saved.')
        self.pump(lambda: bool(self.workflow.state.warnings))
        self.assertEqual(self.workflow.state.warnings, ['Password could not be saved.'])
        self.assertFalse(self.workflow.state.error)
        self.refresh()
        self.assertEqual(self.workflow.state.warnings, ['Password could not be saved.'])
        self.workflow.disconnect()
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertFalse(self.workflow.state.warnings)

    def test_refresh_external_changes_and_reopen(self):
        self.refresh()
        self.assertEqual(self.workflow.state.status.state, 'disconnected')
        self.cli.current = Status('connected', 'Office VPN')
        self.refresh()
        self.assertEqual(self.workflow.state.status.state, 'connected')
        self.workflow.close()
        self.assertEqual(self.cli.current.state, 'connected')
        self.workflow = Workflow(self.callbacks.put, lambda _: None, self.challenges.append, self.cli)
        self.refresh()
        self.assertEqual(self.workflow.state.status.state, 'connected')

    def test_authentication_and_disconnect(self):
        self.workflow.connect(Profile('Office VPN'), 'worker', False)
        self.pump(lambda: len(self.challenges) == 1)
        self.assertEqual(self.workflow.state.busy, 'awaiting authentication')
        self.challenges[0].respond('secret')
        self.pump(lambda: len(self.challenges) == 2)
        self.challenges[1].respond('code')
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(self.workflow.state.status.state, 'connected')
        self.workflow.disconnect()
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(self.workflow.state.status.state, 'disconnected')

    def test_cancel_waiting_for_password_reconciles_status(self):
        self.workflow.connect(Profile('Office VPN'), '', False)
        self.pump(lambda: bool(self.challenges))
        self.workflow.cancel()
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(self.workflow.state.status.state, 'disconnected')
        self.assertIn('Cancelled', self.workflow.state.error)

    def test_authentication_deadline(self):
        self.cli.interactive_timeout = .15
        self.workflow.connect(Profile('Office VPN'), '', False)
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertIn('timed out', self.workflow.state.error)

    def test_refresh_error_clears_on_recovery(self):
        self.cli.unavailable = True
        self.refresh()
        self.assertEqual(self.workflow.state.status.state, 'unavailable')
        self.cli.unavailable = False
        self.refresh()
        self.assertEqual(self.workflow.state.error, '')

    def test_stale_refresh_cannot_overwrite_completed_operation(self):
        gate = threading.Event()
        entered = threading.Event()
        original = self.workflow._snapshot
        def snapshot():
            if threading.current_thread().name == 'stale-refresh':
                captured = original()
                entered.set()
                gate.wait(1)
                return captured
            return original()
        self.workflow._snapshot = snapshot
        def stale_work():
            threading.current_thread().name = 'stale-refresh'
            return snapshot()
        self.workflow._start('refreshing', stale_work, refresh=True)
        self.assertTrue(entered.wait(1))
        self.workflow._start('connecting', lambda: setattr(self.cli, 'current', Status('connected', 'Office VPN')))
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(self.workflow.state.status.state, 'connected')
        gate.set()
        self.pump(lambda: not self.workflow.refreshing)
        self.assertEqual(self.workflow.state.status.state, 'connected')

    def test_missing_cli_setup_state_recovers_after_installation(self):
        with patch.object(self.cli, 'status', side_effect=CLINotFound('CLI missing')):
            self.refresh()
            self.assertTrue(self.workflow.state.setup_required)
            self.assertEqual(self.workflow.state.status.state, 'unavailable')
        self.refresh()
        self.assertFalse(self.workflow.state.setup_required)
        self.assertEqual(self.workflow.state.status.state, 'disconnected')
        self.cli.unavailable = True
        self.refresh()
        self.assertFalse(self.workflow.state.setup_required)

    def test_completion_feedback_is_once_and_only_after_reconciliation(self):
        events = []
        self.workflow.on_complete = lambda operation, success: events.append((operation, success, self.workflow.state.status.state))
        self.cli.current = Status('connected', 'Office VPN')
        self.workflow.disconnect()
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(events, [('disconnecting', True, 'disconnected')])
        self.refresh()
        self.assertEqual(len(events), 1)
        self.cli.unavailable = True
        self.workflow._start('saving profile', lambda: None)
        self.pump(lambda: self.workflow.state.busy is None)
        self.assertEqual(events[-1], ('saving profile', False, 'unavailable'))

    def test_refresh_does_not_overwrite_active_operation(self):
        self.cli.read_gate = threading.Event()
        self.workflow.refresh()
        self.workflow.connect(Profile('Office VPN'), '', False)
        self.pump(lambda: bool(self.challenges))
        self.cli.read_gate.set()
        self.pump(lambda: not self.workflow.refreshing)
        self.assertEqual(self.workflow.state.busy, 'awaiting authentication')
        self.workflow.cancel()
        self.pump(lambda: self.workflow.state.busy is None)


if __name__ == '__main__':
    unittest.main()
