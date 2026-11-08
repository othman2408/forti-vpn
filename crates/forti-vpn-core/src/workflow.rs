//! Asynchronous operation state machine, independent of any UI toolkit.
//!
//! * The [`Workflow`] lives on the UI's main thread. Every public method is
//!   called there.
//! * Each operation runs [`Backend`] calls on a background thread. That
//!   thread never touches [`State`]; it reports through the [`EventSink`]
//!   with [`Event`]s, which the UI passes back into [`Workflow::handle`] on
//!   the main thread (for GTK: an `async_channel` drained by a
//!   `glib::spawn_future_local` loop).
//! * Methods never call back into the UI. Instead they queue [`Effect`]s,
//!   which the UI drains with [`Workflow::take_effects`] after each call,
//!   so no re-entrant borrow can happen.
//!
//! Behaviour started as a port of `forti_vpn/workflow.py` in the Python
//! 0.1.0 app (tag `v0.1.0`) and its `tests/test_workflow.py`; differences
//! are noted where they are made.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{Backend, OperationContext};
use crate::cancel::CancelToken;
use crate::error::VpnError;
use crate::model::{Profile, ProfileSettings, PromptKind, Secret, Status};

/// The number of diagnostics lines kept in memory.
pub const DIAGNOSTICS_LIMIT: usize = 100;

/// The message shown when a worker thread panics.
pub const UNEXPECTED_ERROR: &str =
    "An unexpected application error occurred. Restart the app and retry.";

/// The failure when a prompt is still unanswered at the operation's deadline.
const AUTHENTICATION_TIMEOUT: &str =
    "Authentication timed out. Retry when you are ready to sign in.";

/// Log lines with this prefix are also shown to the user as warnings.
const WARNING_PREFIX: &str = "Warning: ";

/// How often a waiting prompt checks for cancellation and the deadline.
const ANSWER_POLL: Duration = Duration::from_millis(100);

/// A long-running operation started by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Refreshing,
    Connecting,
    Disconnecting,
    SavingProfile,
    RemovingProfile,
    CheckingBeforeExit,
}

impl Operation {
    /// Lowercase name, matching the Python implementation's strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refreshing => "refreshing",
            Self::Connecting => "connecting",
            Self::Disconnecting => "disconnecting",
            Self::SavingProfile => "saving profile",
            Self::RemovingProfile => "removing profile",
            Self::CheckingBeforeExit => "checking before exit",
        }
    }
}

/// What the app is currently doing. `None` in [`State::busy`] means idle.
/// Refreshes never set `busy`; see [`Workflow::is_refreshing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Busy {
    Operation(Operation),
    /// An operation is waiting for the user to answer a prompt.
    AwaitingAuthentication,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub status: Status,
    pub profiles: Vec<Profile>,
    pub busy: Option<Busy>,
    /// The latest user-facing error, or empty.
    pub error: String,
    /// The operation that produced `error`.
    pub error_origin: Option<Operation>,
    /// The FortiClient executable was not found.
    pub setup_required: bool,
    /// Sanitized, timestamped event lines (`HH:MM:SS text`), newest last,
    /// at most [`DIAGNOSTICS_LIMIT`].
    pub diagnostics: Vec<String>,
    /// Non-fatal warnings for the current operation (deduplicated).
    pub warnings: Vec<String>,
}

/// A prompt waiting for the user. Cloning shares the same reply slot; only
/// the first reply is delivered. Dropping every copy without a reply
/// dismisses the prompt, like `respond(None)`.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub kind: PromptKind,
    responder: SyncSender<Option<Secret>>,
}

impl Challenge {
    /// Create a challenge and the receiver its answer arrives on.
    pub fn new(kind: PromptKind) -> (Self, Receiver<Option<Secret>>) {
        let (responder, receiver) = sync_channel(1);
        (Self { kind, responder }, receiver)
    }

    /// Deliver the answer, or `None` when the prompt was dismissed. Later
    /// replies are ignored.
    pub fn respond(&self, value: Option<Secret>) {
        match self.responder.try_send(value) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Reported by a worker thread; apply with [`Workflow::handle`].
#[derive(Debug)]
pub enum Event {
    /// A diagnostics line. A `Warning: ` prefix also adds a warning.
    Log(String),
    /// The backend asked a question. The worker is blocked until
    /// [`Challenge::respond`] is called or every copy of the challenge is
    /// dropped, the operation is cancelled, or the deadline passes.
    Challenge(Challenge),
    /// The prompt was answered or dismissed; the operation continues
    /// (`busy` returns to the operation).
    Resumed { revision: u64 },
    /// The operation finished and the state was re-read from FortiClient.
    Finished(Finished),
}

/// The outcome of an operation, produced on the worker thread.
#[derive(Debug)]
pub struct Finished {
    pub operation: Operation,
    pub revision: u64,
    /// Empty on success.
    pub error: String,
    pub setup_required: bool,
    pub status: Status,
    /// `None` keeps the profiles already shown.
    pub profiles: Option<Vec<Profile>>,
}

/// Work for the UI, queued by workflow methods.
#[derive(Debug, Clone)]
pub enum Effect {
    /// State changed; re-render.
    Changed,
    /// Show a prompt for this challenge.
    Prompt(Challenge),
    /// A non-refresh operation completed, after the state was reconciled.
    /// Emitted exactly once per operation.
    Completed { operation: Operation, success: bool },
}

/// Delivers worker events to the main thread. Must not block.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Returns the local wall-clock time as `HH:MM:SS` for diagnostics lines.
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

pub struct Workflow {
    backend: Arc<dyn Backend>,
    sink: EventSink,
    clock: Clock,
    state: State,
    cancel: CancelToken,
    closed: bool,
    refreshing: bool,
    deadline: Instant,
    operation: Option<Operation>,
    revision: u64,
    effects: Vec<Effect>,
}

impl Workflow {
    pub fn new(backend: Arc<dyn Backend>, sink: EventSink, clock: Clock) -> Self {
        Self {
            backend,
            sink,
            clock,
            state: State::default(),
            cancel: CancelToken::new(),
            closed: false,
            refreshing: false,
            deadline: Instant::now(),
            operation: None,
            revision: 0,
            effects: Vec::new(),
        }
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Direct state access for UI flows that set a message without an
    /// operation (for example "Finish or cancel the current operation").
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The current operation's cancellation token.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Re-read status and profiles. Ignored while busy, refreshing, or closed.
    pub fn refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.start(
            Operation::Refreshing,
            Box::new(|backend, _| Snapshot::read(backend).map(Some)),
        );
    }

    pub fn connect(&mut self, profile: Profile, username: String, remember: bool) {
        self.start(
            Operation::Connecting,
            Box::new(move |backend, context| {
                backend
                    .connect(&profile, &username, remember, context)
                    .map(|_| None)
            }),
        );
    }

    pub fn disconnect(&mut self) {
        self.start(
            Operation::Disconnecting,
            Box::new(|backend, _| backend.disconnect().map(|_| None)),
        );
    }

    pub fn save(&mut self, settings: ProfileSettings, existing: Option<Profile>) {
        self.start(
            Operation::SavingProfile,
            Box::new(move |backend, context| {
                backend
                    .save_profile(&settings, existing.as_ref(), context)
                    .map(|_| None)
            }),
        );
    }

    pub fn remove(&mut self, profile: Profile) {
        self.start(
            Operation::RemovingProfile,
            Box::new(move |backend, _| backend.remove(&profile).map(|()| None)),
        );
    }

    /// Reconcile the state before quitting (no FortiClient changes).
    pub fn check_before_exit(&mut self) {
        self.start(Operation::CheckingBeforeExit, Box::new(|_, _| Ok(None)));
    }

    /// Cancel the current operation (and any prompt it waits on).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Stop delivering events and cancel the current operation. An
    /// established VPN connection is not touched.
    pub fn close(&mut self) {
        self.closed = true;
        self.cancel.cancel();
    }

    /// Add a diagnostics line from the main thread.
    pub fn log(&mut self, text: &str) {
        self.handle(Event::Log(text.to_owned()));
    }

    /// Apply an event from a worker thread.
    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Challenge(challenge) => self.present(challenge),
            // The window is going away; workers only need to wind down.
            _ if self.closed => {}
            Event::Log(text) => self.add_log(&text),
            Event::Resumed { revision } => self.resume(revision),
            Event::Finished(finished) => self.finish(finished),
        }
    }

    /// Drain queued effects, oldest first.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn start(&mut self, operation: Operation, work: Work) {
        if self.state.busy.is_some() || self.closed {
            return;
        }
        if operation == Operation::Refreshing {
            self.refreshing = true;
        } else {
            self.state.busy = Some(Busy::Operation(operation));
            self.operation = Some(operation);
            self.revision += 1;
            self.state.error.clear();
            self.state.warnings.clear();
        }
        self.cancel = CancelToken::new();
        self.deadline = deadline_after(self.backend.interactive_timeout());
        self.effects.push(Effect::Changed);
        let worker = Worker {
            backend: Arc::clone(&self.backend),
            sink: Arc::clone(&self.sink),
            cancel: self.cancel.clone(),
            deadline: self.deadline,
            operation,
            revision: self.revision,
        };
        let spawned = thread::Builder::new()
            .name(operation.as_str().to_owned())
            .spawn(move || worker.run(work));
        if spawned.is_err() {
            // Without a worker nothing would ever report back and the app
            // would stay busy forever.
            self.finish(Finished {
                operation,
                revision: self.revision,
                error: UNEXPECTED_ERROR.to_owned(),
                setup_required: false,
                status: Status::unavailable(),
                profiles: None,
            });
        }
    }

    fn present(&mut self, challenge: Challenge) {
        if self.closed || self.cancel.is_cancelled() {
            // Python only skipped the prompt and let the worker notice the
            // cancellation; dismissing it releases the worker without
            // waiting for its next poll.
            challenge.respond(None);
            return;
        }
        self.state.busy = Some(Busy::AwaitingAuthentication);
        self.effects.push(Effect::Changed);
        self.effects.push(Effect::Prompt(challenge));
    }

    fn resume(&mut self, revision: u64) {
        // A late resume must never make a finished operation busy again.
        if revision != self.revision || self.state.busy.is_none() {
            return;
        }
        self.state.busy = self.operation.map(Busy::Operation);
        self.effects.push(Effect::Changed);
    }

    fn add_log(&mut self, text: &str) {
        if let Some(warning) = text.strip_prefix(WARNING_PREFIX)
            && !self.state.warnings.iter().any(|known| known == warning)
        {
            self.state.warnings.push(warning.to_owned());
        }
        self.push_diagnostic(text);
        self.effects.push(Effect::Changed);
    }

    fn finish(&mut self, finished: Finished) {
        let refresh = finished.operation == Operation::Refreshing;
        if refresh {
            self.refreshing = false;
            // An operation started since this refresh read FortiClient, so
            // its result is stale. Only `is_refreshing` changed.
            if self.state.busy.is_some() || finished.revision != self.revision {
                self.effects.push(Effect::Changed);
                return;
            }
        } else {
            self.state.busy = None;
        }
        self.state.status = finished.status;
        if let Some(profiles) = finished.profiles {
            self.state.profiles = profiles;
        }
        self.state.setup_required = finished.setup_required;
        let success = finished.error.is_empty();
        if !success {
            self.push_diagnostic(&finished.error);
            self.state.error = finished.error;
            self.state.error_origin = Some(finished.operation);
        } else if !refresh || self.state.error_origin == Some(Operation::Refreshing) {
            self.state.error.clear();
            self.state.error_origin = None;
        }
        self.trim_diagnostics();
        self.effects.push(Effect::Changed);
        if !refresh {
            self.effects.push(Effect::Completed {
                operation: finished.operation,
                success,
            });
        }
    }

    fn push_diagnostic(&mut self, text: &str) {
        let line = format!("{} {text}", (self.clock)());
        self.state.diagnostics.push(line);
        self.trim_diagnostics();
    }

    fn trim_diagnostics(&mut self) {
        let diagnostics = &mut self.state.diagnostics;
        let excess = diagnostics.len().saturating_sub(DIAGNOSTICS_LIMIT);
        diagnostics.drain(..excess);
    }
}

/// When an operation starting now times out.
fn deadline_after(timeout: Duration) -> Instant {
    const YEAR: Duration = Duration::from_secs(365 * 24 * 60 * 60);
    let now = Instant::now();
    // An absurd configured timeout must not crash the app by overflowing.
    now.checked_add(timeout).unwrap_or_else(|| now + YEAR)
}

/// A worker thread's job. A refresh returns the state it read; for every
/// other operation the worker reads it afterwards.
type Work = Box<
    dyn FnOnce(&dyn Backend, &OperationContext<'_>) -> Result<Option<Snapshot>, VpnError> + Send,
>;

/// Status and profile details as FortiClient reports them.
struct Snapshot {
    status: Status,
    profiles: Vec<Profile>,
}

impl Snapshot {
    fn read(backend: &dyn Backend) -> Result<Self, VpnError> {
        let status = backend.status()?;
        let profiles = backend
            .list_profiles()?
            .iter()
            .map(|profile| backend.view(profile))
            .collect::<Result<_, _>>()?;
        Ok(Self { status, profiles })
    }
}

/// The part of an operation that runs on its own thread. It never touches
/// the [`Workflow`]; everything it learns goes through the sink.
struct Worker {
    backend: Arc<dyn Backend>,
    sink: EventSink,
    cancel: CancelToken,
    deadline: Instant,
    operation: Operation,
    revision: u64,
}

impl Worker {
    fn run(self, work: Work) {
        let finished = catch_unwind(AssertUnwindSafe(|| self.perform(work)))
            .unwrap_or_else(|_| self.finished(UNEXPECTED_ERROR.to_owned(), false, None));
        (self.sink)(Event::Finished(finished));
    }

    fn perform(&self, work: Work) -> Finished {
        let backend = self.backend.as_ref();
        let ask = |kind: PromptKind| self.ask(kind);
        let log = |text: &str| (self.sink)(Event::Log(text.to_owned()));
        let context = OperationContext {
            cancel: &self.cancel,
            deadline: self.deadline,
            ask: &ask,
            log: &log,
        };
        let result = work(backend, &context).and_then(|read| match read {
            Some(snapshot) => Ok(snapshot),
            None => Snapshot::read(backend),
        });
        let (error, snapshot) = match result {
            Ok(snapshot) => (String::new(), Ok(snapshot)),
            // Show what the failure left behind, not what the app last knew.
            Err(error) => (error.message().to_owned(), Snapshot::read(backend)),
        };
        match snapshot {
            Ok(snapshot) => self.finished(error, false, Some(snapshot)),
            Err(snapshot_error) => self.finished(error, snapshot_error.is_not_found(), None),
        }
    }

    /// `snapshot` is `None` when FortiClient could not be read: the status
    /// is then unavailable and the profiles already shown are kept.
    fn finished(
        &self,
        error: String,
        setup_required: bool,
        snapshot: Option<Snapshot>,
    ) -> Finished {
        let (status, profiles) = match snapshot {
            Some(Snapshot { status, profiles }) => (status, Some(profiles)),
            None => (Status::unavailable(), None),
        };
        Finished {
            operation: self.operation,
            revision: self.revision,
            error,
            setup_required,
            status,
            profiles,
        }
    }

    /// Show a prompt and block until it is answered or dismissed, the
    /// operation is cancelled, or the deadline passes.
    fn ask(&self, kind: PromptKind) -> Result<Option<Secret>, VpnError> {
        let (challenge, answers) = Challenge::new(kind);
        (self.sink)(Event::Challenge(challenge));
        loop {
            let now = Instant::now();
            if self.cancel.is_cancelled() || now >= self.deadline {
                break;
            }
            let slice = ANSWER_POLL.min(self.deadline.saturating_duration_since(now));
            let answer = match answers.recv_timeout(slice) {
                Ok(answer) => answer,
                Err(RecvTimeoutError::Timeout) => continue,
                // Every copy of the challenge is gone, so no answer can come:
                // the prompt was dismissed without a reply.
                Err(RecvTimeoutError::Disconnected) => None,
            };
            // Answered or dismissed, the prompt is over: the UI must leave
            // "Awaiting authentication…" now, not when the operation ends.
            (self.sink)(Event::Resumed {
                revision: self.revision,
            });
            return Ok(answer);
        }
        if Instant::now() >= self.deadline {
            self.cancel.cancel();
            return Err(VpnError::failed(AUTHENTICATION_TIMEOUT));
        }
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! An in-memory FortiClient and an event pump, shared by the workflow and
    //! presentation tests: a port of `FakeCLI` and `pump()` in the Python
    //! `tests/test_workflow.py`.
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{Challenge, Clock, Effect, Event, EventSink, Operation, Workflow};
    use crate::backend::{Backend, OperationContext};
    use crate::cancel::CancelToken;
    use crate::error::VpnError;
    use crate::model::{ClientInfo, ConnectionState, Profile, ProfileSettings, PromptKind, Status};

    /// The fixed time every diagnostics line starts with.
    pub(crate) const CLOCK: &str = "12:00:00";

    /// What the fake FortiClient reports; tests change it between steps.
    pub(crate) struct FakeState {
        pub(crate) interactive_timeout: Duration,
        pub(crate) current: Status,
        /// `status` fails as if FortiClient were not responding.
        pub(crate) unavailable: bool,
        /// `status` fails as if the executable were missing.
        pub(crate) missing: bool,
        /// `list_profiles` waits (up to a second) until this is cancelled.
        pub(crate) read_gate: Option<CancelToken>,
        pub(crate) connect_error: Option<VpnError>,
        pub(crate) disconnect_error: Option<VpnError>,
    }

    pub(crate) struct FakeCli(Mutex<FakeState>);

    impl FakeCli {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self(Mutex::new(FakeState {
                interactive_timeout: Duration::from_secs(1),
                current: Status::new(ConnectionState::Disconnected),
                unavailable: false,
                missing: false,
                read_gate: None,
                connect_error: None,
                disconnect_error: None,
            })))
        }

        pub(crate) fn with<T>(&self, change: impl FnOnce(&mut FakeState) -> T) -> T {
            change(&mut self.0.lock().unwrap())
        }
    }

    impl Backend for FakeCli {
        fn interactive_timeout(&self) -> Duration {
            self.with(|fake| fake.interactive_timeout)
        }

        fn status(&self) -> Result<Status, VpnError> {
            self.with(|fake| {
                if fake.missing {
                    Err(VpnError::not_found("CLI missing"))
                } else if fake.unavailable {
                    Err(VpnError::failed("FortiClient unavailable"))
                } else {
                    Ok(fake.current.clone())
                }
            })
        }

        fn list_profiles(&self) -> Result<Vec<Profile>, VpnError> {
            if let Some(gate) = self.with(|fake| fake.read_gate.clone()) {
                gate.wait_timeout(Duration::from_secs(1));
            }
            Ok(vec![Profile::new("Office VPN")])
        }

        fn view(&self, profile: &Profile) -> Result<Profile, VpnError> {
            Ok(profile.clone())
        }

        fn connect(
            &self,
            profile: &Profile,
            _username: &str,
            _remember: bool,
            context: &OperationContext<'_>,
        ) -> Result<Status, VpnError> {
            if let Some(error) = self.with(|fake| fake.connect_error.clone()) {
                return Err(error);
            }
            if (context.ask)(PromptKind::Password)?.is_none() {
                return Err(VpnError::cancelled("Cancelled"));
            }
            (context.ask)(PromptKind::Mfa)?;
            Ok(self.with(|fake| {
                fake.current = Status::with_profile(ConnectionState::Connected, &profile.name);
                fake.current.clone()
            }))
        }

        fn disconnect(&self) -> Result<Status, VpnError> {
            self.with(|fake| {
                if let Some(error) = fake.disconnect_error.clone() {
                    return Err(error);
                }
                fake.current = Status::new(ConnectionState::Disconnected);
                Ok(fake.current.clone())
            })
        }

        fn save_profile(
            &self,
            settings: &ProfileSettings,
            _existing: Option<&Profile>,
            _context: &OperationContext<'_>,
        ) -> Result<Profile, VpnError> {
            Ok(Profile::new(&settings.name))
        }

        fn remove(&self, _profile: &Profile) -> Result<(), VpnError> {
            Ok(())
        }

        fn information(&self) -> ClientInfo {
            ClientInfo::unavailable()
        }
    }

    /// A workflow whose worker events are applied on the test thread, the
    /// way the GTK main loop applies them.
    pub(crate) struct Harness {
        pub(crate) cli: Arc<FakeCli>,
        pub(crate) workflow: Workflow,
        events: Receiver<Event>,
        /// Prompts shown, oldest first, kept alive like open dialogs.
        pub(crate) challenges: Vec<Challenge>,
        /// Each `Completed` effect, with the status when it was drained.
        pub(crate) completed: Vec<(Operation, bool, ConnectionState)>,
        /// `Debug` output of every event applied.
        pub(crate) transcript: Vec<String>,
    }

    impl Harness {
        pub(crate) fn new(cli: Arc<FakeCli>) -> Self {
            let (sender, events) = channel();
            let sink: EventSink = Arc::new(move |event| {
                // The receiver only goes away when the test is over.
                let _ = sender.send(event);
            });
            let clock: Clock = Arc::new(|| CLOCK.to_owned());
            let workflow = Workflow::new(cli.clone(), sink, clock);
            Self {
                cli,
                workflow,
                events,
                challenges: Vec::new(),
                completed: Vec::new(),
                transcript: Vec::new(),
            }
        }

        /// Apply effects and worker events until `until` holds. Fails after
        /// two seconds.
        pub(crate) fn pump(&mut self, until: impl Fn(&Self) -> bool) {
            let end = Instant::now() + Duration::from_secs(2);
            loop {
                self.apply_effects();
                if until(self) {
                    return;
                }
                assert!(
                    Instant::now() < end,
                    "workflow did not reach the expected state"
                );
                if let Ok(event) = self.events.recv_timeout(Duration::from_millis(20)) {
                    self.apply(event);
                }
            }
        }

        pub(crate) fn refresh(&mut self) {
            self.workflow.refresh();
            self.pump(|harness| !harness.workflow.is_refreshing());
        }

        /// Wait until no operation is running.
        pub(crate) fn settle(&mut self) {
            self.pump(|harness| harness.workflow.state().busy.is_none());
        }

        /// The next worker event, without applying it.
        pub(crate) fn next_event(&self) -> Event {
            self.events.recv_timeout(Duration::from_secs(2)).unwrap()
        }

        pub(crate) fn apply(&mut self, event: Event) {
            self.transcript.push(format!("{event:?}"));
            self.workflow.handle(event);
        }

        fn apply_effects(&mut self) {
            for effect in self.workflow.take_effects() {
                match effect {
                    Effect::Changed => {}
                    Effect::Prompt(challenge) => self.challenges.push(challenge),
                    Effect::Completed { operation, success } => {
                        let status = self.workflow.state().status.state;
                        self.completed.push((operation, success, status));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{CLOCK, FakeCli, Harness};
    use super::*;
    use crate::model::ConnectionState;

    fn answer(value: &str) -> Option<Secret> {
        Some(Secret::new(value.to_owned()))
    }

    fn office() -> Profile {
        Profile::new("Office VPN")
    }

    fn connected() -> Status {
        Status::with_profile(ConnectionState::Connected, "Office VPN")
    }

    #[test]
    fn warning_events_surface_without_connection_failure() {
        let mut harness = Harness::new(FakeCli::new());
        harness
            .workflow
            .log("Warning: Password could not be saved.");
        harness.pump(|harness| !harness.workflow.state().warnings.is_empty());
        assert_eq!(
            harness.workflow.state().warnings,
            ["Password could not be saved."]
        );
        assert!(harness.workflow.state().error.is_empty());
        harness.refresh();
        assert_eq!(
            harness.workflow.state().warnings,
            ["Password could not be saved."]
        );
        harness.workflow.disconnect();
        harness.settle();
        assert!(harness.workflow.state().warnings.is_empty());
    }

    #[test]
    fn repeated_warnings_are_shown_once_and_every_line_is_logged() {
        let mut harness = Harness::new(FakeCli::new());
        harness
            .workflow
            .log("Warning: Password could not be saved.");
        harness
            .workflow
            .log("Warning: Password could not be saved.");
        harness.workflow.log("FortiClient requested password.");
        let state = harness.workflow.state();
        assert_eq!(state.warnings, ["Password could not be saved."]);
        assert_eq!(
            state.diagnostics,
            [
                format!("{CLOCK} Warning: Password could not be saved."),
                format!("{CLOCK} Warning: Password could not be saved."),
                format!("{CLOCK} FortiClient requested password."),
            ]
        );
        assert!(
            harness
                .workflow
                .take_effects()
                .iter()
                .all(|effect| matches!(effect, Effect::Changed))
        );
    }

    #[test]
    fn diagnostics_keep_only_the_newest_lines() {
        let mut harness = Harness::new(FakeCli::new());
        for line in 0..150 {
            harness.workflow.log(&format!("event {line}"));
        }
        let diagnostics = &harness.workflow.state().diagnostics;
        assert_eq!(diagnostics.len(), DIAGNOSTICS_LIMIT);
        assert_eq!(diagnostics[0], format!("{CLOCK} event 50"));
        assert_eq!(
            diagnostics[DIAGNOSTICS_LIMIT - 1],
            format!("{CLOCK} event 149")
        );
    }

    #[test]
    fn refresh_external_changes_and_reopen() {
        let cli = FakeCli::new();
        let mut harness = Harness::new(cli.clone());
        harness.refresh();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Disconnected
        );
        cli.with(|fake| fake.current = connected());
        harness.refresh();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Connected
        );
        harness.workflow.close();
        assert_eq!(
            cli.with(|fake| fake.current.state),
            ConnectionState::Connected
        );
        let mut reopened = Harness::new(cli);
        reopened.refresh();
        assert_eq!(
            reopened.workflow.state().status.state,
            ConnectionState::Connected
        );
    }

    #[test]
    fn authentication_and_disconnect() {
        let mut harness = Harness::new(FakeCli::new());
        harness
            .workflow
            .connect(office(), "worker".to_owned(), false);
        harness.pump(|harness| harness.challenges.len() == 1);
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::AwaitingAuthentication)
        );
        harness.challenges[0].respond(answer("secret"));
        harness.pump(|harness| harness.challenges.len() == 2);
        harness.challenges[1].respond(answer("code"));
        harness.settle();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Connected
        );
        let kinds: Vec<_> = harness
            .challenges
            .iter()
            .map(|challenge| challenge.kind)
            .collect();
        assert_eq!(kinds, [PromptKind::Password, PromptKind::Mfa]);
        harness.workflow.disconnect();
        harness.settle();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn answering_returns_busy_to_the_operation() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| harness.challenges.len() == 1);
        harness.challenges[0].respond(answer("secret"));
        let resumed = harness.next_event();
        assert!(matches!(resumed, Event::Resumed { revision: 1 }));
        harness.apply(resumed);
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::Operation(Operation::Connecting))
        );
        harness.pump(|harness| harness.challenges.len() == 2);
        harness.challenges[1].respond(None);
        harness.settle();
    }

    #[test]
    fn cancel_waiting_for_password_reconciles_status() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        harness.workflow.cancel();
        harness.settle();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Disconnected
        );
        assert!(harness.workflow.state().error.contains("Cancelled"));
    }

    #[test]
    fn authentication_deadline() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.interactive_timeout = Duration::from_millis(150));
        let mut harness = Harness::new(cli);
        harness.workflow.connect(office(), String::new(), false);
        harness.settle();
        assert!(harness.workflow.state().error.contains("timed out"));
        assert_eq!(harness.workflow.state().error, AUTHENTICATION_TIMEOUT);
        assert!(harness.workflow.cancel_token().is_cancelled());
    }

    #[test]
    fn dropped_prompts_count_as_dismissed() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        harness.challenges.clear();
        harness.settle();
        assert_eq!(harness.workflow.state().error, "Cancelled");
    }

    #[test]
    fn dropped_prompts_resume_the_operation_like_a_dismissal() {
        let transcripts = [false, true].map(|dropped| {
            let mut harness = Harness::new(FakeCli::new());
            harness.workflow.connect(office(), String::new(), false);
            harness.pump(|harness| !harness.challenges.is_empty());
            if dropped {
                harness.challenges.clear();
            } else {
                harness.challenges[0].respond(None);
            }
            let resumed = harness.next_event();
            assert!(
                matches!(resumed, Event::Resumed { revision: 1 }),
                "{resumed:?}"
            );
            harness.apply(resumed);
            assert_eq!(
                harness.workflow.state().busy,
                Some(Busy::Operation(Operation::Connecting))
            );
            harness.settle();
            harness.transcript
        });
        assert_eq!(transcripts[0], transcripts[1]);
    }

    #[test]
    fn refresh_error_clears_on_recovery() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.unavailable = true);
        let mut harness = Harness::new(cli.clone());
        harness.refresh();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Unavailable
        );
        assert_eq!(harness.workflow.state().error, "FortiClient unavailable");
        assert_eq!(
            harness.workflow.state().error_origin,
            Some(Operation::Refreshing)
        );
        cli.with(|fake| fake.unavailable = false);
        harness.refresh();
        assert_eq!(harness.workflow.state().error, "");
        assert_eq!(harness.workflow.state().error_origin, None);
    }

    #[test]
    fn successful_refresh_keeps_an_operation_error() {
        let cli = FakeCli::new();
        cli.with(|fake| {
            fake.current = connected();
            fake.disconnect_error = Some(VpnError::failed("failure"));
        });
        let mut harness = Harness::new(cli);
        harness.workflow.disconnect();
        harness.settle();
        let state = harness.workflow.state();
        assert_eq!(state.error, "failure");
        assert_eq!(state.error_origin, Some(Operation::Disconnecting));
        assert_eq!(state.status, connected());
        assert_eq!(state.diagnostics, [format!("{CLOCK} failure")]);
        harness.refresh();
        assert_eq!(harness.workflow.state().error, "failure");
        assert_eq!(
            harness.completed,
            [(Operation::Disconnecting, false, ConnectionState::Connected)]
        );
    }

    #[test]
    fn stale_refresh_cannot_overwrite_completed_operation() {
        let cli = FakeCli::new();
        let mut harness = Harness::new(cli.clone());
        let entered = CancelToken::new();
        let gate = CancelToken::new();
        let (reached, release) = (entered.clone(), gate.clone());
        harness.workflow.start(
            Operation::Refreshing,
            Box::new(move |backend, _| {
                let captured = Snapshot::read(backend)?;
                reached.cancel();
                release.wait_timeout(Duration::from_secs(1));
                Ok(Some(captured))
            }),
        );
        assert!(entered.wait_timeout(Duration::from_secs(1)));
        harness.workflow.start(
            Operation::Connecting,
            Box::new(move |_, _| {
                cli.with(|fake| fake.current = connected());
                Ok(None)
            }),
        );
        harness.settle();
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Connected
        );
        gate.cancel();
        harness.pump(|harness| !harness.workflow.is_refreshing());
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Connected
        );
    }

    #[test]
    fn missing_cli_setup_state_recovers_after_installation() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.missing = true);
        let mut harness = Harness::new(cli.clone());
        harness.refresh();
        assert!(harness.workflow.state().setup_required);
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Unavailable
        );
        cli.with(|fake| fake.missing = false);
        harness.refresh();
        assert!(!harness.workflow.state().setup_required);
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Disconnected
        );
        cli.with(|fake| fake.unavailable = true);
        harness.refresh();
        assert!(!harness.workflow.state().setup_required);
    }

    #[test]
    fn completion_feedback_is_once_and_only_after_reconciliation() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.current = connected());
        let mut harness = Harness::new(cli.clone());
        harness.workflow.disconnect();
        harness.settle();
        assert_eq!(
            harness.completed,
            [(
                Operation::Disconnecting,
                true,
                ConnectionState::Disconnected
            )]
        );
        harness.refresh();
        assert_eq!(harness.completed.len(), 1);
        cli.with(|fake| fake.unavailable = true);
        harness
            .workflow
            .start(Operation::SavingProfile, Box::new(|_, _| Ok(None)));
        harness.settle();
        assert_eq!(
            harness.completed.last(),
            Some(&(
                Operation::SavingProfile,
                false,
                ConnectionState::Unavailable
            ))
        );
        assert_eq!(harness.completed.len(), 2);
    }

    #[test]
    fn completion_is_queued_after_the_state_update() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.check_before_exit();
        assert!(matches!(
            harness.workflow.take_effects().as_slice(),
            [Effect::Changed]
        ));
        let event = harness.next_event();
        assert!(matches!(event, Event::Finished(_)));
        harness.apply(event);
        let effects = harness.workflow.take_effects();
        assert!(matches!(
            effects.as_slice(),
            [
                Effect::Changed,
                Effect::Completed {
                    operation: Operation::CheckingBeforeExit,
                    success: true
                }
            ]
        ));
        assert_eq!(harness.workflow.state().busy, None);
        assert_eq!(
            harness.workflow.state().status.state,
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn refresh_does_not_overwrite_active_operation() {
        let cli = FakeCli::new();
        let gate = CancelToken::new();
        cli.with(|fake| fake.read_gate = Some(gate.clone()));
        let mut harness = Harness::new(cli);
        harness.workflow.refresh();
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        gate.cancel();
        harness.pump(|harness| !harness.workflow.is_refreshing());
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::AwaitingAuthentication)
        );
        harness.workflow.cancel();
        harness.settle();
    }

    #[test]
    fn refresh_and_operations_are_ignored_while_busy_or_closed() {
        let cli = FakeCli::new();
        let gate = CancelToken::new();
        cli.with(|fake| fake.read_gate = Some(gate.clone()));
        let mut harness = Harness::new(cli);
        harness.workflow.refresh();
        assert!(harness.workflow.is_refreshing());
        assert_eq!(harness.workflow.state().busy, None);
        assert_eq!(harness.workflow.revision, 0);
        harness.workflow.refresh();
        assert_eq!(harness.workflow.take_effects().len(), 1);
        gate.cancel();
        harness.pump(|harness| !harness.workflow.is_refreshing());

        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        harness.workflow.refresh();
        harness.workflow.disconnect();
        assert!(!harness.workflow.is_refreshing());
        assert_eq!(harness.workflow.revision, 1);
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::AwaitingAuthentication)
        );
        harness.workflow.cancel();
        harness.settle();

        harness.workflow.close();
        harness.workflow.refresh();
        harness.workflow.check_before_exit();
        assert!(!harness.workflow.is_refreshing());
        assert_eq!(harness.workflow.state().busy, None);
        assert!(harness.workflow.take_effects().is_empty());
    }

    #[test]
    fn operations_clear_the_previous_error_and_warnings() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.unavailable = true);
        let mut harness = Harness::new(cli.clone());
        harness.refresh();
        harness
            .workflow
            .log("Warning: Password could not be saved.");
        cli.with(|fake| fake.unavailable = false);
        harness.workflow.check_before_exit();
        let state = harness.workflow.state();
        assert_eq!(
            state.busy,
            Some(Busy::Operation(Operation::CheckingBeforeExit))
        );
        assert!(state.error.is_empty());
        assert!(state.warnings.is_empty());
        assert_eq!(state.error_origin, Some(Operation::Refreshing));
        harness.settle();
        assert_eq!(harness.workflow.state().error_origin, None);
    }

    #[test]
    fn worker_panics_are_reported_as_unexpected_errors() {
        let mut harness = Harness::new(FakeCli::new());
        harness.refresh();
        harness.workflow.start(
            Operation::SavingProfile,
            Box::new(|_, _| panic!("backend bug")),
        );
        harness.settle();
        let state = harness.workflow.state();
        assert_eq!(state.error, UNEXPECTED_ERROR);
        assert_eq!(state.error_origin, Some(Operation::SavingProfile));
        assert_eq!(state.status, Status::unavailable());
        assert_eq!(state.profiles, [office()]);
        assert!(!state.setup_required);
        assert_eq!(
            harness.completed,
            [(
                Operation::SavingProfile,
                false,
                ConnectionState::Unavailable
            )]
        );
    }

    #[test]
    fn failed_reads_keep_the_profiles_shown() {
        let cli = FakeCli::new();
        let mut harness = Harness::new(cli.clone());
        harness.refresh();
        cli.with(|fake| fake.missing = true);
        harness.workflow.remove(office());
        harness.settle();
        let state = harness.workflow.state();
        assert_eq!(state.profiles, [office()]);
        assert_eq!(state.status, Status::unavailable());
        assert!(state.setup_required);
        assert_eq!(state.error, "CLI missing");
        assert_eq!(state.error_origin, Some(Operation::RemovingProfile));
    }

    #[test]
    fn challenge_answers_never_reach_events_or_state() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.start(
            Operation::Connecting,
            Box::new(|_, context| {
                (context.log)("FortiClient requested password.");
                let password = (context.ask)(PromptKind::Password)?;
                (context.log)("FortiClient requested mfa.");
                let code = (context.ask)(PromptKind::Mfa)?;
                let delivered = password
                    .as_deref()
                    .is_some_and(|value| value == "hunter2-pass")
                    && code.as_deref().is_some_and(|value| value == "481516");
                Err(VpnError::failed(if delivered {
                    "FortiClient rejected the credentials."
                } else {
                    "The answers did not arrive."
                }))
            }),
        );
        harness.pump(|harness| harness.challenges.len() == 1);
        harness.challenges[0].respond(answer("hunter2-pass"));
        harness.pump(|harness| harness.challenges.len() == 2);
        harness.challenges[1].respond(answer("481516"));
        harness.settle();
        let state = harness.workflow.state();
        assert_eq!(state.error, "FortiClient rejected the credentials.");
        assert_eq!(state.diagnostics.len(), 3);
        let everything = format!("{state:?} {:?}", harness.transcript);
        for secret in ["hunter2-pass", "481516"] {
            assert!(!everything.contains(secret), "{secret} leaked");
        }
    }

    #[test]
    fn stale_resumes_and_cancelled_prompts_are_ignored() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        harness.workflow.handle(Event::Resumed { revision: 0 });
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::AwaitingAuthentication)
        );
        harness.workflow.cancel();
        let (challenge, answers) = Challenge::new(PromptKind::Mfa);
        harness.workflow.handle(Event::Challenge(challenge));
        assert_eq!(answers.try_recv(), Ok(None));
        assert!(harness.workflow.take_effects().is_empty());
        harness.settle();
        harness.workflow.handle(Event::Resumed { revision: 1 });
        assert_eq!(harness.workflow.state().busy, None);
    }

    #[test]
    fn later_replies_to_a_challenge_are_ignored() {
        let (challenge, answers) = Challenge::new(PromptKind::Password);
        let copy = challenge.clone();
        challenge.respond(answer("first"));
        copy.respond(answer("second"));
        challenge.respond(None);
        assert_eq!(answers.try_recv(), Ok(answer("first")));
        assert!(answers.try_recv().is_err());
    }

    #[test]
    fn nothing_is_applied_after_close() {
        let mut harness = Harness::new(FakeCli::new());
        harness.workflow.connect(office(), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        harness.workflow.close();
        assert!(harness.workflow.is_closed());
        assert!(harness.workflow.cancel_token().is_cancelled());
        loop {
            let event = harness.next_event();
            let finished = matches!(event, Event::Finished(_));
            harness.apply(event);
            if finished {
                break;
            }
        }
        harness.workflow.log("Late line");
        let state = harness.workflow.state();
        assert_eq!(state.busy, Some(Busy::AwaitingAuthentication));
        assert!(state.error.is_empty());
        assert!(state.diagnostics.is_empty());
        assert!(harness.workflow.take_effects().is_empty());
        let (challenge, answers) = Challenge::new(PromptKind::Password);
        harness.workflow.handle(Event::Challenge(challenge));
        assert_eq!(answers.try_recv(), Ok(None));
        assert!(harness.workflow.take_effects().is_empty());
    }

    #[test]
    fn absurd_timeouts_do_not_overflow_the_deadline() {
        let before = Instant::now();
        assert!(deadline_after(Duration::MAX) > before);
        let deadline = deadline_after(Duration::from_secs(1));
        assert!(deadline >= before + Duration::from_secs(1));
    }
}
